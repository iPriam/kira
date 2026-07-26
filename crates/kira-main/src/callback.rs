//! Running a VM program whose foreign half can call back into it.
//!
//! A `@FFI.Callback` value is the address of a generated entry thunk in the
//! adapter sidecar. When C calls through it, the thunk marshals its arguments
//! and asks for a Kira function to be run — and the only thing that can run one
//! under `--backend vm` is the interpreter that is already several frames up the
//! stack, inside the foreign call C was reached from.
//!
//! # Why a session, and why a thread-local
//!
//! The interpreter is borrowed for the whole run, so the way back in cannot be a
//! second `&mut` to it. It is a *fresh* run instead: [`Program::call`] takes
//! `&self` and gives the nested call its own heap and operand stack, so the two
//! never share mutable state. That is what makes the crossing safe rather than
//! merely arranged.
//!
//! The invoker the thunk calls is a bare `extern "C" fn` with no user-data
//! pointer, so it cannot close over the session; it finds it in a thread-local
//! installed for the run's duration. A thunk called from a thread the host never
//! entered therefore finds nothing, and says so instead of running against a
//! null pointer. This is the same shape `kira-hybrid-runtime` uses for the
//! native half calling a `@Runtime` function — the two paths differ only in
//! which library carries the thunk.

use std::cell::{Cell, RefCell};
use std::path::Path;

use kira_dynamic_ffi::{ForeignAdapterError, ForeignAdapterLibrary, RuntimeInvoker};
use kira_runtime_abi::{
    BridgeData, BridgeValue, ForeignAggregates, ForeignArg, ForeignCallError, ForeignResult,
    HostCapabilities, NativeArg, NativeResult, NativeStateError, NativeStateStore,
    NativeStateToken, NativeStateTypeId, NativeStateValue,
};
use kira_vm_runtime::{Program, RunOutcome, VmError};

use crate::ForeignBinding;

thread_local! {
    /// The session the invoker on this thread should call back into.
    ///
    /// Null when no session is running here, which is a case this has to be able
    /// to represent: a C library may call a callback from anywhere.
    static ACTIVE_SESSION: Cell<*const ForeignSession> = const { Cell::new(std::ptr::null()) };
}

/// A VM program, its adapter sidecar, and the state they share for one run.
///
/// Owns both halves so each nesting level can borrow them: the host handed to
/// the interpreter carries nothing of its own, so a callback's nested run gets a
/// fresh one without any `&mut` being aliased.
pub struct ForeignSession {
    program: Program,
    library: ForeignAdapterLibrary,
    imports: Vec<ForeignBinding>,
    callbacks: Vec<String>,
    /// Callback state, shared across nesting levels.
    ///
    /// A callback that recovers state boxed by the run that installed it has to
    /// find the same store; a per-level store would lose it at the boundary,
    /// which is exactly the case native callbacks exist for.
    state: RefCell<NativeStateStore>,
}

impl ForeignSession {
    /// Loads the sidecar at `sidecar` and binds it to `program`.
    ///
    /// `callbacks` names the entry thunk of each callback row, in id order, so
    /// the host resolves the symbol the backend defined rather than spelling the
    /// contract a second time.
    pub fn load(
        program: Program,
        sidecar: &Path,
        imports: Vec<ForeignBinding>,
        callbacks: Vec<String>,
        aggregates: ForeignAggregates,
    ) -> Result<ForeignSession, ForeignAdapterError> {
        let library = ForeignAdapterLibrary::load(sidecar, aggregates)?;
        Ok(ForeignSession {
            program,
            library,
            imports,
            callbacks,
            state: RefCell::new(NativeStateStore::new()),
        })
    }

    /// Runs the program's entrypoint with the foreign half live.
    ///
    /// The invoker is installed for exactly this call and cleared afterwards, so
    /// the sidecar never holds a callback that outlives the session it reaches.
    pub fn run(&self) -> Result<RunOutcome, VmError> {
        let _active = ActiveSession::install(self);
        let mut host = SessionHost { session: self };
        self.program.run(&mut host)
    }

    /// Calls one foreign import through its generated adapter.
    fn call_foreign(
        &self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let binding = self
            .imports
            .get(foreign_id as usize)
            .ok_or(ForeignCallError::NoForeignHost)?;
        self.library
            .call(&binding.adapter_symbol, &binding.signature, args)
            .map_err(|error| match error {
                ForeignAdapterError::Call(call) => call,
                // A sidecar that cannot answer at all is the same condition as
                // having no foreign half: the detail is reported separately, and
                // the VM's channel carries a typed reason, not a sentence.
                _ => ForeignCallError::NoForeignHost,
            })
    }

    /// The address of one callback's entry thunk.
    fn callback_address(&self, callback_id: u32) -> Result<u64, ForeignCallError> {
        let symbol = self
            .callbacks
            .get(callback_id as usize)
            .ok_or(ForeignCallError::NoForeignHost)?;
        self.library
            .callback_address(symbol)
            .map_err(|_| ForeignCallError::NoForeignHost)
    }
}

/// A [`HostCapabilities`] over a shared session.
///
/// Carries nothing itself, which is what lets a nested run build another one
/// while the outer one is still borrowed by the interpreter.
struct SessionHost<'a> {
    session: &'a ForeignSession,
}

impl HostCapabilities for SessionHost<'_> {
    fn write_line(&mut self, text: &str) {
        println!("{text}");
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        self.session.call_foreign(foreign_id, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.session.callback_address(callback_id)
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        self.session.state.borrow_mut().create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session.state.borrow().recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.session.state.borrow_mut().replace(token, ty, value)
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.session.state.borrow_mut().free(token)
    }
}

/// Marks a session as this thread's for as long as it is alive.
struct ActiveSession<'a> {
    session: &'a ForeignSession,
    previous: *const ForeignSession,
}

impl<'a> ActiveSession<'a> {
    fn install(session: &'a ForeignSession) -> ActiveSession<'a> {
        let previous = ACTIVE_SESSION.replace(session);
        let installed: Option<RuntimeInvoker> = Some(invoke_runtime);
        // SAFETY: `invoke_runtime` is a `'static` function, so it stays callable
        // for the process's life, and `Drop` clears it before this session's
        // borrow ends regardless. A sidecar with no thunks has no installer to
        // call, which is why the result is dropped rather than reported: there
        // is nothing to install into and nothing that will ever call back.
        let _ = unsafe { session.library.install_callback_invoker(installed) };
        ActiveSession { session, previous }
    }
}

impl Drop for ActiveSession<'_> {
    fn drop(&mut self) {
        // SAFETY: the run has returned, so no thunk of this sidecar is on the
        // stack and nothing can be mid-callback.
        let _ = unsafe { self.session.library.install_callback_invoker(None) };
        ACTIVE_SESSION.set(self.previous);
    }
}

/// The C-to-interpreter direction: what a generated entry thunk calls.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), and `out` at one writable [`BridgeValue`].
unsafe extern "C" fn invoke_runtime(
    function_id: u32,
    args: *const BridgeValue,
    count: u32,
    out: *mut BridgeValue,
) {
    let pointer = ACTIVE_SESSION.get();
    if pointer.is_null() {
        fatal(&format!(
            "a C callback entered Kira function {function_id} from a thread with no running \
             program; callbacks are supported only on the thread that started the run"
        ));
    }
    // SAFETY: the pointer is non-null, so an `ActiveSession` guard is alive on
    // this thread and borrows the session it points at for at least as long as
    // this call — the guard lives across the whole run, and this call is reached
    // from inside it.
    let session = unsafe { &*pointer };

    let values: &[BridgeValue] = if count == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `count` readable values at `args`.
        unsafe { std::slice::from_raw_parts(args, count as usize) }
    };
    let arguments: Vec<NativeArg<'_>> = values.iter().map(|value| scalar_arg(*value)).collect();

    let mut host = SessionHost { session };
    match session.program.call(&mut host, function_id, &arguments) {
        Ok(result) => {
            // SAFETY: the caller guarantees `out` is one writable value.
            unsafe { *out = scalar_result(result) };
        }
        // A trap has nowhere to go from here: unwinding out of an `extern "C"`
        // frame aborts, and the C caller has no error channel. Report and exit
        // as the runtime's own traps do, so a trap reached through a callback
        // and one reached directly look the same to a user.
        Err(trap) => fatal(&format!("runtime trap: {trap}")),
    }
}

/// One callback argument, which is always a scalar.
///
/// A callback signature carries fixed-width scalars, `Bool`, and `RawPtr` — the
/// frontend refuses everything else — so a string or a handle arriving here is a
/// generated thunk and a signature that disagree, not a program.
fn scalar_arg(value: BridgeValue) -> NativeArg<'static> {
    match value.decode() {
        Some(BridgeData::Void) => NativeArg::Void,
        Some(BridgeData::Int(value)) => NativeArg::Int(value),
        Some(BridgeData::Float(value)) => NativeArg::Float(value),
        Some(BridgeData::Bool(value)) => NativeArg::Bool(value),
        Some(BridgeData::RawPtr(value)) => NativeArg::RawPtr(value),
        _ => fatal("a C callback passed an argument this seam does not carry"),
    }
}

/// One callback result, on the same terms.
fn scalar_result(result: NativeResult) -> BridgeValue {
    match result {
        NativeResult::Void => BridgeValue::VOID,
        NativeResult::Int(value) => BridgeValue::encode(BridgeData::Int(value)),
        NativeResult::Float(value) => BridgeValue::encode(BridgeData::Float(value)),
        NativeResult::Bool(value) => BridgeValue::encode(BridgeData::Bool(value)),
        NativeResult::RawPtr(value) => BridgeValue::encode(BridgeData::RawPtr(value)),
        NativeResult::Str(_) | NativeResult::Handle(_) => {
            fatal("a Kira callback returned a value this seam does not carry")
        }
    }
}

/// Reports a condition the seam cannot return from, and exits.
fn fatal(message: &str) -> ! {
    eprintln!("kira: {message}");
    std::process::exit(1);
}
