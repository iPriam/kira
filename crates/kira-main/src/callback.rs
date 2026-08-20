//! Running a VM program whose foreign half can call back into it.
//!
//! A `@FFI.Callback` value is the address of a libffi closure this session
//! prepared. When C calls through it, the closure marshals its arguments and
//! asks for a Kira function to be run — and the only thing that can run one
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
//! native half calling a `@Runtime` function.

use std::cell::Cell;
use std::ffi::{CStr, c_char, c_void};
use std::path::Path;
use std::pin::Pin;
use std::ptr;
use std::sync::{
    Mutex,
    atomic::{AtomicUsize, Ordering},
};

use kira_dynamic_ffi::{ForeignLibrary, ForeignLibraryError};
use kira_libffi::{FfiClosure, LibffiRuntime, RawFfiCif};
use kira_runtime_abi::{
    FileRequest, FileResponse, FileSystemError, ForeignAggregates, ForeignArg, ForeignCallError,
    ForeignResult, ForeignSignature, ForeignType, ForeignTypeSpec, HostCapabilities, LinuxSyscall,
    NativeArg, NativeResult, NativeStateError, NativeStatePathStep, NativeStateStore,
    NativeStateToken, NativeStateTypeId, NativeStateValue, SyscallError, file_system, syscall,
};
use kira_vm_runtime::{Program, RunOutcome, VmError, debug::VmDebugObserver};

use crate::{ForeignBinding, ForeignBindingTarget};

thread_local! {
    /// The session the invoker on this thread should call back into.
    ///
    /// Null when no session is running here, which is a case this has to be able
    /// to represent: a C library may call a callback from anywhere.
    static ACTIVE_SESSION: Cell<*const ForeignSession> = const { Cell::new(ptr::null()) };
}

static FFI_DEBUG_COUNT: AtomicUsize = AtomicUsize::new(0);

/// A VM program, its native libraries, and the state they share for one run.
///
/// Owns both halves so each nesting level can borrow them: the host handed to
/// the interpreter carries nothing of its own, so a callback's nested run gets a
/// fresh one without any `&mut` being aliased.
pub struct ForeignSession {
    program: Program,
    libraries: Vec<ForeignLibrary>,
    /// The marshalling engine, loaded only for a program that needs one.
    ///
    /// A `@FFI.Syscall` reaches the kernel by instruction: there is no address
    /// for libffi to call through and no shared object for the bundled one to
    /// be found in. Loading it anyway made a program whose only foreign calls
    /// are system calls refuse to start on a machine with no `libffi.so.8` —
    /// which is the same dependency the native backend already declines to put
    /// on such a program's link line.
    libffi: Option<LibffiRuntime>,
    imports: Vec<ForeignBinding>,
    callback_signatures: Vec<ForeignSignature>,
    callback_registry: Mutex<CallbackRegistry>,
    aggregates: ForeignAggregates,
    /// Callback state, shared across nesting levels.
    ///
    /// A callback that recovers state boxed by the run that installed it has to
    /// find the same store; a per-level store would lose it at the boundary,
    /// which is exactly the case native callbacks exist for.
    state: Mutex<NativeStateStore>,
}

impl ForeignSession {
    /// Opens the program's native libraries and prepares libffi callback
    /// closures for every callback row.
    pub fn load_dynamic(
        program: Program,
        imports: Vec<ForeignBinding>,
        callbacks: Vec<ForeignSignature>,
        aggregates: ForeignAggregates,
    ) -> Result<ForeignSession, ForeignLibraryError> {
        Self::load_dynamic_inner(program, imports, callbacks, aggregates, None)
    }

    /// Opens direct foreign bindings with libffi staged beside a live bundle.
    pub fn load_dynamic_with_runtime_path(
        program: Program,
        imports: Vec<ForeignBinding>,
        callbacks: Vec<ForeignSignature>,
        aggregates: ForeignAggregates,
        runtime_path: impl AsRef<Path>,
    ) -> Result<ForeignSession, ForeignLibraryError> {
        Self::load_dynamic_inner(
            program,
            imports,
            callbacks,
            aggregates,
            Some(runtime_path.as_ref().to_path_buf()),
        )
    }

    fn load_dynamic_inner(
        program: Program,
        imports: Vec<ForeignBinding>,
        callbacks: Vec<ForeignSignature>,
        aggregates: ForeignAggregates,
        runtime_path: Option<std::path::PathBuf>,
    ) -> Result<ForeignSession, ForeignLibraryError> {
        let mut libraries = Vec::new();
        for binding in &imports {
            match &binding.target {
                ForeignBindingTarget::Library { path, .. } => {
                    if libraries.iter().any(|library: &ForeignLibrary| {
                        !library.is_process() && library.path() == path
                    }) {
                        continue;
                    }
                    let library = match runtime_path.as_deref() {
                        Some(runtime_path) => ForeignLibrary::load_with_runtime_path(
                            path,
                            aggregates.clone(),
                            runtime_path,
                        ),
                        None => ForeignLibrary::load(path, aggregates.clone()),
                    }?;
                    libraries.push(library);
                }
                ForeignBindingTarget::Process { .. } => {
                    if libraries.iter().any(ForeignLibrary::is_process) {
                        continue;
                    }
                    let library = match runtime_path.as_deref() {
                        Some(runtime_path) => ForeignLibrary::load_process_with_runtime_path(
                            aggregates.clone(),
                            runtime_path,
                        ),
                        None => ForeignLibrary::load_process(aggregates.clone()),
                    }?;
                    libraries.push(library);
                }
                // Neither of these opens anything: one has no artifact to open
                // and the other is an instruction, not a library.
                ForeignBindingTarget::Unavailable | ForeignBindingTarget::Syscall { .. } => {}
            }
        }
        // Asked rather than assumed: a library or process binding is called
        // through libffi and a callback is a libffi closure, but a program whose
        // whole foreign surface is system calls needs neither, and demanding the
        // engine anyway would refuse to start on the machines this capability
        // exists for.
        let needs_libffi = !callbacks.is_empty()
            || imports.iter().any(|binding| {
                matches!(
                    binding.target,
                    ForeignBindingTarget::Library { .. } | ForeignBindingTarget::Process { .. }
                )
            });
        let libffi = match (needs_libffi, runtime_path.as_deref()) {
            (false, _) => None,
            (true, Some(runtime_path)) => Some(LibffiRuntime::load_from(runtime_path)?),
            (true, None) => Some(LibffiRuntime::load()?),
        };
        let callback_closures = (0..callbacks.len()).map(|_| None).collect();
        Ok(ForeignSession {
            program,
            libraries,
            libffi,
            imports,
            callback_signatures: callbacks,
            callback_registry: Mutex::new(CallbackRegistry {
                closures: callback_closures,
                contexts: Vec::new(),
            }),
            aggregates,
            state: Mutex::new(NativeStateStore::new()),
        })
    }

    /// Runs the program's entrypoint with the foreign half live.
    ///
    /// The session is installed as this thread's for exactly this call and
    /// cleared afterwards, so no callback reaches a session that has ended.
    pub fn run(&self) -> Result<RunOutcome, VmError> {
        self.run_inner(None)
    }

    /// Runs the VM entrypoint with an instruction-level debugger attached.
    pub fn run_with_debug(
        &self,
        observer: &mut dyn VmDebugObserver,
    ) -> Result<RunOutcome, VmError> {
        self.run_inner(Some(observer))
    }

    fn run_inner(&self, observer: Option<&mut dyn VmDebugObserver>) -> Result<RunOutcome, VmError> {
        let _active = ActiveSession::install(self);
        let mut host = SessionHost { session: self };
        match observer {
            Some(observer) => self.program.run_with_debug(&mut host, observer),
            None => self.program.run(&mut host),
        }
    }

    /// The system call and signature of import `foreign_id`, when it names one.
    ///
    /// The session answers what it *is* and the host does it, because a kernel
    /// entry belongs to the host's process rather than to the libraries and
    /// closures this session owns.
    fn syscall_binding(&self, foreign_id: u32) -> Option<(LinuxSyscall, ForeignSignature)> {
        let binding = self.imports.get(foreign_id as usize)?;
        Some((binding.syscall_target()?, binding.signature.clone()))
    }

    /// Calls one foreign import through libffi or a legacy adapter.
    fn call_foreign(
        &self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let binding = self
            .imports
            .get(foreign_id as usize)
            .ok_or(ForeignCallError::NoForeignHost)?;
        if std::env::var_os("KIRA_DEBUG_FFI").is_some()
            && FFI_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed) < 64
        {
            eprintln!("ffi call {foreign_id}: {:?}", binding.target);
        }
        // A data symbol is bound and then nothing is invoked, in a callback
        // session exactly as on the main one.
        if binding.answers_address {
            let symbol = match &binding.target {
                ForeignBindingTarget::Library { symbol, .. }
                | ForeignBindingTarget::Process { symbol } => symbol.clone(),
                _ => return Err(ForeignCallError::NoForeignHost),
            };
            let wanted_process = matches!(binding.target, ForeignBindingTarget::Process { .. });
            let Some(library) = self
                .libraries
                .iter()
                .find(|library| library.is_process() == wanted_process)
            else {
                return Err(ForeignCallError::NoForeignHost);
            };
            return library
                .symbol_address(&symbol)
                .map(|address| ForeignResult::RawPtr(address as u64))
                .map_err(|_| ForeignCallError::NoForeignHost);
        }
        match &binding.target {
            ForeignBindingTarget::Library { path, symbol } => {
                let Some(library) = self
                    .libraries
                    .iter()
                    .find(|library| !library.is_process() && library.path() == path)
                else {
                    return Err(refused(
                        foreign_id,
                        &format!("native library `{}` was not opened", path.display()),
                    ));
                };
                // SAFETY: the binding was resolved from the package declaration
                // and the library was opened for this session with the same
                // aggregate table.
                unsafe { library.call(symbol, &binding.signature, args) }.map_err(|error| {
                    match error {
                        ForeignLibraryError::Call(call) => call,
                        other => refused(foreign_id, &other.to_string()),
                    }
                })
            }
            ForeignBindingTarget::Process { symbol } => {
                let Some(library) = self.libraries.iter().find(|library| library.is_process())
                else {
                    return Err(refused(
                        foreign_id,
                        &format!(
                            "`{symbol}` is a host-process binding and the image was not opened"
                        ),
                    ));
                };
                // SAFETY: the process binding uses the declaration's exact
                // LibFFI signature.
                unsafe { library.call(symbol, &binding.signature, args) }.map_err(|error| {
                    match error {
                        ForeignLibraryError::Call(call) => call,
                        other => refused(foreign_id, &other.to_string()),
                    }
                })
            }
            ForeignBindingTarget::Unavailable => Err(refused(
                foreign_id,
                "the declaring library resolved to no artifact for this target",
            )),
            // The host answered this before reaching the session; see
            // `syscall_binding`. Reaching it means a host called this directly
            // rather than through `HostCapabilities::call_foreign`, and it has
            // no kernel entry of its own to serve the call with.
            ForeignBindingTarget::Syscall { call } => Err(refused(
                foreign_id,
                &format!(
                    "`{}` is a system call, which this session does not enter — its host does",
                    call.label()
                ),
            )),
        }
    }

    /// The address of one callback's libffi closure.
    fn callback_address(&self, callback_id: u32) -> Result<u64, ForeignCallError> {
        let index = callback_id as usize;
        let signature = self
            .callback_signatures
            .get(index)
            .cloned()
            .ok_or(ForeignCallError::NoForeignHost)?;
        let mut registry = self
            .callback_registry
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        if let Some(closure) = registry.closures.get(index).and_then(Option::as_ref) {
            return Ok(closure.code() as usize as u64);
        }
        if registry.closures.get(index).is_none() {
            return Err(ForeignCallError::NoForeignHost);
        }
        // A program with a callback row loaded the engine above, so a missing
        // one here is a session built from a table that disagrees with itself.
        let runtime = self
            .libffi
            .as_ref()
            .ok_or(ForeignCallError::NoForeignHost)?;
        let context = Box::pin(CallbackContext {
            function_id: self
                .program
                .module()
                .foreign_callbacks
                .get(index)
                .ok_or(ForeignCallError::NoForeignHost)?
                .function(),
            signature: signature.clone(),
        });
        let user_data = (&*context as *const CallbackContext).cast_mut().cast();
        // SAFETY: `context` is moved into the locked callback registry after
        // preparation, so its boxed address remains valid for the closure. The
        // registry lock is held only across preparation and insertion;
        // `FfiClosure::new` cannot invoke user or foreign code.
        let closure = unsafe {
            FfiClosure::new(
                runtime,
                &signature,
                &self.aggregates,
                ffi_callback_entry,
                user_data,
            )
        }
        .map_err(|_| ForeignCallError::NoForeignHost)?;
        let address = closure.code() as usize as u64;
        registry.contexts.push(context);
        let Some(slot) = registry.closures.get_mut(index) else {
            return Err(ForeignCallError::NoForeignHost);
        };
        *slot = Some(closure);
        Ok(address)
    }
}

/// Owns callback closures and their pinned user-data in one lock domain.
///
/// A context is pinned because its address is what libffi hands the callback
/// entry: it must stay valid until the registry drops, whatever the vector
/// does as it grows.
struct CallbackRegistry {
    closures: Vec<Option<FfiClosure>>,
    contexts: Vec<Pin<Box<CallbackContext>>>,
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

    /// Reaches the process's own filesystem, exactly as [`crate::StdoutHost`]
    /// does.
    ///
    /// This host serves a VM run that also has a foreign half, which is the
    /// only thing that distinguishes it — and nothing about having one narrows
    /// what the program may read. Leaving it to the default refused every file
    /// operation in exactly the programs most likely to open one.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(file_system::perform(request))
    }

    /// Enters this process's kernel, exactly as [`crate::StdoutHost`] reaches
    /// this process's filesystem.
    ///
    /// The same grant, on the same reasoning: this host stands in the process a
    /// `--backend vm` run happens in, so the descriptors the program writes to
    /// are this process's. Which calls it may serve is not this host's decision
    /// — [`syscall::call`] applies the policy the call itself carries, and the
    /// CLI refuses a non-servable one by name before the program starts.
    fn syscall(&mut self, call: LinuxSyscall, args: &[i64]) -> Result<i64, SyscallError> {
        // SAFETY: the words came from a `@FFI.Syscall` call site the frontend
        // validated to register-width scalars, and a pointer among them is one
        // this program produced — the obligation this session already carries
        // for every pointer it hands a C library through libffi.
        unsafe { syscall::perform(call, args) }
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        // Served here rather than on the session, because entering the kernel is
        // the *host's* capability: the session owns libraries and closures, and
        // a system call needs neither.
        if let Some((call, signature)) = self.session.syscall_binding(foreign_id) {
            return syscall::call(self, call, &signature, args);
        }
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
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .create(ty, value)
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .recover(token, ty)
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .replace(token, ty, value)
    }

    // The path-addressed operations, forwarded to the same store. Without these
    // the trait's defaults answer by recovering — a deep copy of the whole state
    // per field read and two per write — which is the difference between a UI
    // frame costing its own work and costing its glyph cache on every access.
    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .check(token, ty)
    }

    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .read_at(token, ty, path)
            .cloned()
    }

    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        *self
            .session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .write_at(token, ty, path)? = value;
        Ok(())
    }

    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match self
            .session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .write_at(token, ty, path)?
        {
            // The elements are shared with whoever last read this array, so the
            // append buys a block of its own before it lands.
            NativeStateValue::Array(elements) => std::sync::Arc::make_mut(elements).push(value),
            _ => return Err(NativeStateError::PathMismatch),
        }
        Ok(())
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        self.session
            .state
            .lock()
            .unwrap_or_else(|held| held.into_inner())
            .free(token)
    }
}

/// Marks a session as this thread's for as long as it is alive.
struct ActiveSession {
    previous: *const ForeignSession,
}

impl ActiveSession {
    fn install(session: &ForeignSession) -> ActiveSession {
        let previous = ACTIVE_SESSION.replace(session);
        ActiveSession { previous }
    }
}

impl Drop for ActiveSession {
    fn drop(&mut self) {
        ACTIVE_SESSION.set(self.previous);
    }
}

/// The immutable data a libffi closure needs to enter one Kira function.
struct CallbackContext {
    function_id: u32,
    signature: ForeignSignature,
}

/// The C-to-interpreter entry used by a bundled libffi closure.
unsafe extern "C" fn ffi_callback_entry(
    _cif: *mut RawFfiCif,
    result: *mut c_void,
    arguments: *mut *mut c_void,
    user_data: *mut c_void,
) {
    if user_data.is_null() {
        fatal("a libffi callback has no Kira function context");
    }
    // SAFETY: the context is a boxed value retained by `ForeignSession` until
    // its closure is dropped, and libffi only calls this while that closure is live.
    let context = unsafe { &*(user_data.cast::<CallbackContext>()) };
    if std::env::var_os("KIRA_DEBUG_FFI").is_some()
        && FFI_DEBUG_COUNT.fetch_add(1, Ordering::Relaxed) < 64
    {
        eprintln!(
            "ffi callback function={} params={:?}",
            context.function_id, context.signature
        );
    }
    let session_pointer = ACTIVE_SESSION.get();
    if session_pointer.is_null() {
        fatal(&format!(
            "a C callback entered Kira function {} without a running program",
            context.function_id
        ));
    }
    // SAFETY: `ActiveSession` installs this pointer for the complete VM run and
    // clears it only after the C call and all nested callbacks have returned.
    let session = unsafe { &*session_pointer };
    let count = context.signature.parameters().len();
    if count != 0 && arguments.is_null() {
        fatal("libffi supplied no argument array for a non-empty callback");
    }
    let pointers: &[*mut c_void] = if count == 0 {
        &[]
    } else {
        // SAFETY: libffi supplies one pointer for every prepared CIF parameter.
        unsafe { std::slice::from_raw_parts(arguments.cast_const(), count) }
    };
    let mut strings: Vec<Option<String>> = (0..count).map(|_| None).collect();
    for (index, (spec, pointer)) in context
        .signature
        .parameters()
        .iter()
        .copied()
        .zip(pointers.iter().copied())
        .enumerate()
    {
        if pointer.is_null() {
            fatal(&format!("libffi supplied a null argument slot {index}"));
        }
        if let ForeignTypeSpec::Scalar(ForeignType::CString) = spec {
            // SAFETY: `pointer` addresses the C pointer word libffi decoded for
            // this parameter. The pointed-to bytes are valid for this callback.
            let address = unsafe { ptr::read_unaligned(pointer.cast::<*const c_char>()) };
            let text = if address.is_null() {
                String::new()
            } else {
                // SAFETY: a CString callback parameter is NUL-terminated by its
                // C caller and remains live through this synchronous callback.
                unsafe { String::from_utf8_lossy(CStr::from_ptr(address).to_bytes()) }.into_owned()
            };
            strings[index] = Some(text);
        }
    }
    let native_arguments: Vec<NativeArg<'_>> = context
        .signature
        .parameters()
        .iter()
        .copied()
        .zip(pointers.iter().copied())
        .enumerate()
        .map(|(index, (spec, pointer))| callback_argument(spec, pointer, &strings[index]))
        .collect();
    let mut host = SessionHost { session };
    match session
        .program
        .call(&mut host, context.function_id, &native_arguments)
    {
        Ok(value) => write_callback_result(context.signature.result(), value, result),
        Err(trap) => fatal(&format!("runtime trap in C callback: {trap}")),
    }
}

fn callback_argument<'a>(
    spec: ForeignTypeSpec,
    pointer: *mut c_void,
    string: &'a Option<String>,
) -> NativeArg<'a> {
    match spec {
        ForeignTypeSpec::Aggregate(_) => {
            // C passes an aggregate by value, so libffi points directly at its
            // temporary bytes. The Kira callback contract receives that address.
            NativeArg::RawPtr(pointer as usize as u64)
        }
        ForeignTypeSpec::Scalar(ty) => match ty {
            ForeignType::Void => NativeArg::Void,
            ForeignType::I8 => NativeArg::Int(i64::from(read_unaligned::<i8>(pointer))),
            ForeignType::I16 => NativeArg::Int(i64::from(read_unaligned::<i16>(pointer))),
            ForeignType::I32 => NativeArg::Int(i64::from(read_unaligned::<i32>(pointer))),
            ForeignType::I64 => NativeArg::Int(read_unaligned::<i64>(pointer)),
            ForeignType::U8 => NativeArg::Int(i64::from(read_unaligned::<u8>(pointer))),
            ForeignType::U16 => NativeArg::Int(i64::from(read_unaligned::<u16>(pointer))),
            ForeignType::U32 => NativeArg::Int(i64::from(read_unaligned::<u32>(pointer))),
            ForeignType::U64 => NativeArg::Int(read_unaligned::<u64>(pointer) as i64),
            ForeignType::Bool => NativeArg::Bool(read_unaligned::<u8>(pointer) != 0),
            ForeignType::F32 => NativeArg::Float(f64::from(read_unaligned::<f32>(pointer))),
            ForeignType::F64 => NativeArg::Float(read_unaligned::<f64>(pointer)),
            ForeignType::RawPtr => NativeArg::RawPtr(read_unaligned::<usize>(pointer) as u64),
            ForeignType::CString => match string.as_deref() {
                Some(value) => NativeArg::Str(value),
                None => fatal("a CString callback argument was not decoded"),
            },
        },
    }
}

fn write_callback_result(spec: ForeignTypeSpec, result: NativeResult, output: *mut c_void) {
    if spec == ForeignTypeSpec::Scalar(ForeignType::Void) {
        if !matches!(result, NativeResult::Void) {
            fatal("a void C callback returned a value");
        }
        return;
    }
    if output.is_null() {
        fatal("libffi supplied no result storage for a non-void callback");
    }
    match (spec, result) {
        (ForeignTypeSpec::Scalar(ForeignType::I8), NativeResult::Int(value)) => {
            write_unaligned(output, value as i8)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I16), NativeResult::Int(value)) => {
            write_unaligned(output, value as i16)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I32), NativeResult::Int(value)) => {
            write_unaligned(output, value as i32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::I64), NativeResult::Int(value)) => {
            write_unaligned(output, value)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U8), NativeResult::Int(value)) => {
            write_unaligned(output, value as u8)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U16), NativeResult::Int(value)) => {
            write_unaligned(output, value as u16)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U32), NativeResult::Int(value)) => {
            write_unaligned(output, value as u32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::U64), NativeResult::Int(value)) => {
            write_unaligned(output, value as u64)
        }
        (ForeignTypeSpec::Scalar(ForeignType::Bool), NativeResult::Bool(value)) => {
            write_unaligned(output, u8::from(value))
        }
        (ForeignTypeSpec::Scalar(ForeignType::F32), NativeResult::Float(value)) => {
            write_unaligned(output, value as f32)
        }
        (ForeignTypeSpec::Scalar(ForeignType::F64), NativeResult::Float(value)) => {
            write_unaligned(output, value)
        }
        (ForeignTypeSpec::Scalar(ForeignType::RawPtr), NativeResult::RawPtr(value)) => {
            if (value as usize) as u64 != value {
                fatal("a callback returned a pointer wider than the target");
            }
            write_unaligned(output, value as usize)
        }
        _ => fatal("a Kira callback returned a value with the wrong C type"),
    }
}

fn read_unaligned<T: Copy>(pointer: *mut c_void) -> T {
    // SAFETY: libffi supplies a pointer to initialized storage for the type
    // described by the callback CIF; unaligned access handles all valid C layouts.
    unsafe { ptr::read_unaligned(pointer.cast::<T>()) }
}

fn write_unaligned<T: Copy>(pointer: *mut c_void, value: T) {
    // SAFETY: libffi supplies writable storage sized for the callback result CIF.
    unsafe { ptr::write_unaligned(pointer.cast::<T>(), value) };
}

/// Names the import a foreign call could not reach, and why.
///
/// The trap this becomes carries no room for either — a `ForeignCallError`
/// crosses the seam as a tag — and "this host has no adapter" sends a reader
/// looking through every declaration in the program.
fn refused(foreign_id: u32, reason: &str) -> ForeignCallError {
    eprintln!("kira: foreign import {foreign_id} cannot be called: {reason}");
    ForeignCallError::NoForeignHost
}

/// Reports a condition the seam cannot return from, and exits.
fn fatal(message: &str) -> ! {
    eprintln!("kira: {message}");
    std::process::exit(1);
}
