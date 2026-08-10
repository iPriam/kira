//! A loaded hybrid program, and the two directions it runs in.
//!
//! A [`Session`] owns both halves: the [`Program`] the VM runs and the
//! [`NativeLibrary`] the machine code lives in. It is the embedder for the
//! first and the host for the second, which is what lets the two call each
//! other without either learning about the other's world.
//!
//! # Why the host holds no mutable state
//!
//! The VM takes its host as `&mut dyn HostCapabilities` and holds it for the
//! whole run. Native code called *through* that borrow can call back and needs
//! a host again — so a single long-lived `&mut` host would have to be aliased,
//! which it may not be. The way out is to make the host worth nothing: [`Host`]
//! is a handle over a shared `&Session` and carries no state of its own, so a
//! fresh one is built per nesting level and the `&mut` is only ever to the
//! handle. `write_line` goes straight to stdout, so there is nothing to
//! synchronize between levels.
//!
//! # Why the session is found through a thread-local
//!
//! The invoker the library calls back through is a bare `extern "C" fn` with no
//! user-data pointer, so it cannot close over the session. It finds it in a
//! thread-local instead, set for the run's duration by a guard and cleared when
//! the guard drops. Native code calling back from a thread the host never
//! entered therefore finds nothing — and says so and exits, rather than running
//! against a null pointer.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, RwLock};

use kira_bytecode::module::Module;
use kira_hybrid_definition::HybridManifest;
use kira_runtime_abi::{
    BridgeValue, Execution, FileRequest, FileResponse, FileSystemError, ForeignArg,
    ForeignCallError, ForeignResult, HostCapabilities, NativeArg, NativeCallError, NativeReturn,
    NativeStateError, NativeStatePathStep, NativeStateStore, NativeStateToken, NativeStateTypeId,
    NativeStateValue, file_system, native_state_walk, native_state_walk_mut,
};
use kira_vm_runtime::Program;

use crate::error::HybridError;
use crate::foreign;
use crate::library::NativeLibrary;
use crate::marshal::{self, OwnedArg};
use crate::validate;

thread_local! {
    /// The session the invoker on this thread should call back into.
    ///
    /// Null when no session is running here, which is the case this must be
    /// able to represent: the library may call back from anywhere.
    static ACTIVE_SESSION: Cell<*const Session> = const { Cell::new(std::ptr::null()) };
}

/// A loaded hybrid program: its bytecode half, its native half, and the
/// manifest tying them together.
pub struct Session {
    manifest: HybridManifest,
    /// The program used for the next VM entry or native callback.
    ///
    /// A graphics app's entrypoint is suspended inside the native window loop
    /// for most of its lifetime. The native callback thunks can still enter the
    /// VM while that happens, so a live VM reload replaces this slot rather
    /// than trying to interrupt the suspended entrypoint. An `Arc` keeps the
    /// program that is already executing alive until its current callback
    /// returns.
    program: RwLock<VmProgram>,
    library: NativeLibrary,
    /// VM callback state for an all-runtime bundle.
    ///
    /// `kira live --backend vm` still needs a native library for its foreign
    /// adapters, so it is transported as a hybrid bundle. Its Kira functions
    /// all run in the VM, though, and using the native bridge's whole-value
    /// state ABI there turns every field read into a copy of the entire UI
    /// state. Keep the VM's path-addressed store for that split; genuine
    /// hybrid programs continue to use the native half's state ABI.
    vm_state: Option<Mutex<NativeStateStore>>,
    /// The generation of the latest program swap, and the generation observed
    /// by a callback after that swap. The runner waits for the latter before it
    /// reports a reload complete, so a committed swap is not mistaken for a
    /// frame that never used the new code.
    observed_generation: AtomicU64,
    observed_wait: Condvar,
    observed_lock: Mutex<()>,
}

/// A validated VM program plus the reload generation it belongs to.
struct VmProgram {
    generation: u64,
    program: Arc<Program>,
    /// Maps function ids baked into the loaded native callback thunks to the
    /// corresponding function in this generation's module.
    callback_ids: Arc<Vec<u32>>,
}

impl Session {
    /// Loads the bundle `manifest_path` describes: both payloads, bound and
    /// proven consistent.
    ///
    /// Payload paths are resolved relative to the manifest's own directory
    /// unless they are absolute, so a built bundle can be moved as a unit.
    pub fn load(manifest_path: &Path) -> Result<Session, HybridError> {
        let bytes = read(manifest_path)?;
        let manifest =
            HybridManifest::from_bytes(&bytes).map_err(|source| HybridError::Manifest {
                path: manifest_path.to_path_buf(),
                source,
            })?;

        let base = manifest_path.parent().unwrap_or(Path::new("."));
        let bytecode_path = resolve(base, &manifest.bytecode_path);
        let library_path = resolve(base, &manifest.native_library_path);

        let module_bytes = read(&bytecode_path)?;
        let module = Module::from_bytes(&module_bytes).map_err(|source| HybridError::Bytecode {
            path: bytecode_path,
            source,
        })?;

        // Prove the halves agree before either is trusted: the manifest is what
        // every crossing marshals against.
        validate::bundle(&manifest, &module)?;

        let program = Arc::new(Program::load(module).map_err(HybridError::Program)?);
        let callback_ids = Arc::new(
            (0..manifest.functions.len())
                .map(|index| index as u32)
                .collect(),
        );
        let vm_state = manifest
            .functions
            .iter()
            .all(|function| function.execution == Execution::Runtime)
            .then(|| Mutex::new(NativeStateStore::new()));
        // The callback rows ride on the bytecode half, which already carries
        // them for the VM's own `ForeignCallback`; the native half is where
        // their thunks live.
        let callbacks = program.module().foreign_callbacks.len();
        let library = NativeLibrary::load(
            &library_path,
            &manifest.functions,
            &manifest.foreign,
            callbacks,
        )?;

        Ok(Session {
            manifest,
            program: RwLock::new(VmProgram {
                generation: 0,
                program,
                callback_ids,
            }),
            library,
            vm_state,
            observed_generation: AtomicU64::new(0),
            observed_wait: Condvar::new(),
            observed_lock: Mutex::new(()),
        })
    }

    /// The manifest this session was loaded from.
    pub fn manifest(&self) -> &HybridManifest {
        &self.manifest
    }

    /// Whether every Kira function in this session is running on the VM.
    ///
    /// The live VM backend still carries a native adapter library when the
    /// program reaches C, but its Kira code is safe to replace at callback
    /// boundaries. A genuinely mixed hybrid session cannot make that promise:
    /// native code may still hold a stack into the old native image.
    pub fn is_vm_only(&self) -> bool {
        self.manifest
            .functions
            .iter()
            .all(|function| function.execution == Execution::Runtime)
    }

    /// Replaces the VM half while preserving the loaded native adapter library
    /// and callback-state store.
    ///
    /// The manifest is intentionally validated again. The loaded native half
    /// remains in place, so a changed function signature, callback table, or
    /// foreign boundary must be rejected and handled by the supervisor's
    /// relaunch path rather than being allowed to cross an old ABI.
    pub fn replace_vm_program(&self, module: Module) -> Result<u64, HybridError> {
        if !self.is_vm_only() {
            return Err(HybridError::Mismatch(
                "a mixed hybrid session cannot replace its VM half while running".to_owned(),
            ));
        }
        let callback_ids = {
            let slot = self.program.read().unwrap_or_else(|held| held.into_inner());
            validate::hot_reload(&self.manifest, slot.program.module(), &module)
                .map_err(|error| HybridError::Mismatch(error.to_string()))?
        };
        let program = Arc::new(Program::load(module).map_err(HybridError::Program)?);
        let mut slot = self
            .program
            .write()
            .unwrap_or_else(|held| held.into_inner());
        let generation = slot.generation.saturating_add(1);
        slot.generation = generation;
        slot.program = program;
        slot.callback_ids = Arc::new(callback_ids);
        // The retained UI must rebuild once after this swap. The marker lives
        // in the loaded adapter library, not in the runner, so the callback
        // consumes the same state even though the swap request came from the
        // protocol thread.
        self.library.mark_live_reload();
        Ok(generation)
    }

    /// Waits until a callback has entered the newly swapped program.
    pub fn wait_for_vm_reload(&self, generation: u64, timeout: std::time::Duration) -> bool {
        if self.observed_generation.load(Ordering::Acquire) >= generation {
            return true;
        }
        let guard = self
            .observed_lock
            .lock()
            .unwrap_or_else(|held| held.into_inner());
        let (guard, _) = self
            .observed_wait
            .wait_timeout_while(guard, timeout, |_| {
                self.observed_generation.load(Ordering::Acquire) < generation
            })
            .unwrap_or_else(|held| held.into_inner());
        drop(guard);
        self.observed_generation.load(Ordering::Acquire) >= generation
    }

    /// Takes the program currently selected for a callback or entrypoint.
    fn current_program(&self) -> (Arc<Program>, u64, Arc<Vec<u32>>) {
        let slot = self.program.read().unwrap_or_else(|held| held.into_inner());
        (
            Arc::clone(&slot.program),
            slot.generation,
            Arc::clone(&slot.callback_ids),
        )
    }

    /// Records that a callback ran through `generation`.
    fn observe_generation(&self, generation: u64) {
        let observed = self.observed_generation.load(Ordering::Acquire);
        if observed < generation {
            self.observed_generation
                .store(generation, Ordering::Release);
            self.observed_wait.notify_all();
        }
    }

    /// Runs the program's entrypoint to completion.
    ///
    /// The entrypoint may live on either engine — `@Main` is annotatable like
    /// anything else — so this dispatches on the engine the manifest records
    /// rather than assuming the bytecode half starts every program.
    pub fn run(&self) -> Result<(), HybridError> {
        let _active = ActiveSession::install(self);
        // The native half of a hybrid program is a shared library, so it has no
        // emitted `main` to report its heap balance from. The host asks on its
        // behalf once the run is over — see `HeapReportAtExit`.
        let _heap_report = HeapReportAtExit::new(&self.library);
        let entry = self
            .manifest
            .entry_function()
            .ok_or(HybridError::NoEntrypoint)?;

        match entry.execution {
            Execution::Native => {
                let trampoline = self.library.trampoline(entry.id).ok_or_else(|| {
                    HybridError::Mismatch(format!(
                        "the entrypoint `{}` is native but bound no trampoline",
                        entry.name,
                    ))
                })?;
                // SAFETY: the trampoline is this library's, and validation
                // proved the entrypoint takes no parameters, so an empty
                // argument array is its signature.
                let out = unsafe { self.library.call(trampoline, &mut []) };
                // SAFETY: `out` is what the trampoline just wrote, and its
                // string handle (if any) is unfreed.
                unsafe { marshal::lift_result(&self.library, out) }.map_err(|error| {
                    HybridError::Mismatch(format!(
                        "the entrypoint `{}` returned a value this runtime cannot read: {error}",
                        entry.name,
                    ))
                })?;
                Ok(())
            }
            _ => {
                let mut host = Host { session: self };
                let (program, _, _) = self.current_program();
                program
                    .run(&mut host)
                    .map(|_| ())
                    .map_err(HybridError::Trap)
            }
        }
    }

    /// Calls one native function: the runtime-to-native direction.
    fn call_native(
        &self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        let trampoline = self
            .library
            .trampoline(function_id)
            .ok_or(NativeCallError::UnboundFunction(function_id))?;

        // The callee frees every string among these; this side must not.
        //
        // Building an aggregate's node tree can fail — it allocates in the
        // native half — so this is where a bad argument is reported, before
        // any trampoline runs on a half-built list.
        let mut lowered = marshal::lower_args(&self.library, args)
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        // SAFETY: the trampoline is this library's, and the VM calls with the
        // module's own arity, which validation proved equals the manifest's —
        // which is the signature the trampoline was emitted for.
        let out = unsafe { self.library.call(trampoline, &mut lowered) };
        // SAFETY: `out` is what the trampoline just wrote, and its string
        // handle (if any) is unfreed.
        let result = unsafe { marshal::lift_result(&self.library, out) }
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;

        // SAFETY: `lowered` is the array that call just wrote through, and no
        // written-through slot has been lifted yet.
        let writebacks = unsafe { marshal::lift_writebacks(&self.library, function_id, &lowered) }
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        Ok(NativeReturn { result, writebacks })
    }

    /// Calls one foreign adapter: the runtime-half `CALL_FOREIGN` direction.
    ///
    /// The adapter lives in the same native half the trampolines do, so this
    /// reaches the one copy of the C library rather than opening a second one.
    /// The import's signature comes from the manifest's foreign table, which the
    /// bundle validation proved matches the bytecode.
    fn call_foreign(
        &self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let adapter = self
            .library
            .adapter(foreign_id)
            .ok_or(ForeignCallError::NoForeignHost)?;
        let import = self
            .manifest
            .foreign
            .get(foreign_id as usize)
            .ok_or(ForeignCallError::NoForeignHost)?;
        // SAFETY: the adapter is this library's, bound by import id, and the
        // signature is the manifest row for that same id.
        unsafe {
            foreign::call_adapter(
                &self.library,
                adapter,
                &import.signature,
                &self.manifest.foreign_aggregates,
                args,
            )
        }
    }
}

/// A [`HostCapabilities`] over a shared session.
///
/// Stateless by design; see this module's docs for why that is what makes
/// nesting work.
struct Host<'a> {
    session: &'a Session,
}

impl HostCapabilities for Host<'_> {
    fn write_line(&mut self, text: &str) {
        // Straight to stdout, exactly as the VM-only host does. Both halves
        // write through Rust's `LineWriter`, which flushes on newline, so
        // output from the two engines interleaves correctly on fd 1 with no
        // extra flushing.
        println!("{text}");
    }

    fn call_native(
        &mut self,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeReturn, NativeCallError> {
        self.session.call_native(function_id, args)
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        self.session.call_foreign(foreign_id, args)
    }

    fn foreign_callback(&mut self, callback_id: u32) -> Result<u64, ForeignCallError> {
        self.session
            .library
            .callback_address(callback_id)
            .ok_or(ForeignCallError::NoForeignHost)
    }

    /// Straight to the process's filesystem, exactly as the VM-only host does.
    ///
    /// The two halves of a hybrid program run in one process, so a `@Runtime`
    /// function and a `@Native` one reach the same files — and through the same
    /// implementation, since `kira_rt_fs_*` calls this very function.
    fn file_system(&mut self, request: FileRequest<'_>) -> Result<FileResponse, FileSystemError> {
        Ok(file_system::perform(request))
    }

    fn native_state_create(
        &mut self,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<NativeStateToken, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .create(ty, value),
            None => self.session.library.native_state_create(ty, value),
        }
    }

    fn native_state_recover(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<NativeStateValue, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .recover(token, ty),
            None => self.session.library.native_state_recover(token, ty),
        }
    }

    fn native_state_replace(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .replace(token, ty, value),
            None => self.session.library.native_state_replace(token, ty, value),
        }
    }

    fn native_state_free(&mut self, token: NativeStateToken) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .free(token),
            None => self.session.library.native_state_free(token),
        }
    }

    fn native_state_check(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .check(token, ty),
            None => self
                .session
                .library
                .native_state_recover(token, ty)
                .map(|_| ()),
        }
    }

    fn native_state_read(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
    ) -> Result<NativeStateValue, NativeStateError> {
        match &self.session.vm_state {
            Some(state) => state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .read_at(token, ty, path)
                .cloned(),
            None => {
                let root = self.session.library.native_state_recover(token, ty)?;
                native_state_walk(&root, path).cloned()
            }
        }
    }

    fn native_state_write(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => {
                *state
                    .lock()
                    .unwrap_or_else(|held| held.into_inner())
                    .write_at(token, ty, path)? = value;
                Ok(())
            }
            None => {
                let mut root = self.session.library.native_state_recover(token, ty)?;
                *native_state_walk_mut(&mut root, path)? = value;
                self.session.library.native_state_replace(token, ty, root)
            }
        }
    }

    fn native_state_append(
        &mut self,
        token: NativeStateToken,
        ty: NativeStateTypeId,
        path: &[NativeStatePathStep],
        value: NativeStateValue,
    ) -> Result<(), NativeStateError> {
        match &self.session.vm_state {
            Some(state) => match state
                .lock()
                .unwrap_or_else(|held| held.into_inner())
                .write_at(token, ty, path)?
            {
                NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
                _ => return Err(NativeStateError::PathMismatch),
            },
            None => {
                let mut root = self.session.library.native_state_recover(token, ty)?;
                match native_state_walk_mut(&mut root, path)? {
                    NativeStateValue::Array(elements) => Arc::make_mut(elements).push(value),
                    _ => return Err(NativeStateError::PathMismatch),
                }
                self.session.library.native_state_replace(token, ty, root)?;
            }
        }
        Ok(())
    }
}

/// Marks a session as this thread's for as long as it is alive.
///
/// Installs the invoker on the way in and clears it on the way out, so the
/// library never holds a callback that outlives the session it would reach.
struct ActiveSession<'a> {
    session: &'a Session,
    previous: *const Session,
}

impl<'a> ActiveSession<'a> {
    fn install(session: &'a Session) -> ActiveSession<'a> {
        let previous = ACTIVE_SESSION.replace(session);
        // SAFETY: `invoke_runtime` is a `'static` function, so it stays
        // callable for the process's life; `Drop` clears it before this
        // session's borrow ends regardless.
        unsafe { session.library.install_invoker(Some(invoke_runtime)) };
        ActiveSession { session, previous }
    }
}

impl Drop for ActiveSession<'_> {
    fn drop(&mut self) {
        // SAFETY: `run` has returned, so no native code of this library is on
        // the stack and nothing can be mid-callback.
        unsafe { self.session.library.install_invoker(None) };
        ACTIVE_SESSION.set(self.previous);
    }
}

/// The native-to-runtime direction: what the library calls back through.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), every string handle among them must be transferred to this
/// call, and `out` must point at one writable [`BridgeValue`].
unsafe extern "C" fn invoke_runtime(
    function_id: u32,
    args: *mut BridgeValue,
    count: u32,
    out: *mut BridgeValue,
) {
    let pointer = ACTIVE_SESSION.get();
    if pointer.is_null() {
        // A hybrid program's native half calling back from a thread the host
        // never entered is out of scope for v0. Say so and stop, rather than
        // running against nothing.
        fatal(&format!(
            "native code called runtime function {function_id} from a thread with no \
             hybrid session; v0 supports callbacks only on the thread that started \
             the program"
        ));
    }
    // SAFETY: the pointer is non-null, so an `ActiveSession` guard is alive on
    // this thread and is borrowing the session it points at for at least as
    // long as this call — the guard lives across the whole `run`, and this
    // call is reached from inside it.
    let session = unsafe { &*pointer };

    let values: &[BridgeValue] = if count == 0 {
        &[]
    } else {
        // SAFETY: the caller guarantees `count` readable values at `args`.
        unsafe { std::slice::from_raw_parts(args, count as usize) }
    };

    // SAFETY: the caller transfers every string handle among the arguments;
    // `take_args` frees each exactly once.
    let owned = match unsafe { marshal::take_args(&session.library, values) } {
        Ok(owned) => owned,
        Err(error) => fatal(&format!(
            "native code called runtime function {function_id} with an argument this \
             runtime cannot read: {error}"
        )),
    };
    let borrowed: Vec<NativeArg<'_>> = owned.iter().map(OwnedArg::borrow).collect();

    // The parameters this function writes through, read off the manifest — the
    // same row the native caller's own signature was generated from.
    let capture: Vec<u16> = session
        .manifest
        .functions
        .get(function_id as usize)
        .map(|function| function.params.as_slice())
        .unwrap_or_default()
        .iter()
        .enumerate()
        .filter(|(_, param)| param.ownership.is_mutable())
        .map(|(slot, _)| slot as u16)
        .collect();

    let mut host = Host { session };
    let (program, generation, callback_ids) = session.current_program();
    let Some(&current_function_id) = callback_ids.get(function_id as usize) else {
        fatal(&format!(
            "native code called runtime function {function_id}, but the live module has no identity for it"
        ));
    };
    match program.call_capturing(&mut host, current_function_id, &borrowed, &capture) {
        Ok(returned) => {
            session.observe_generation(generation);
            // Each written-through parameter's final value replaces the argument
            // that arrived in its slot, exactly as a trampoline does going the
            // other way. The argument's own handle was consumed by `take_args`,
            // so the slot holds nothing to free.
            for (slot, value) in returned.writebacks {
                let replacement = marshal::lower_result(&session.library, value);
                if (slot as usize) < values.len() {
                    // SAFETY: the slot is within `count`, which the caller
                    // guarantees is writable — the manifest's parameter list
                    // and the call's arity are proven equal by bundle
                    // validation, and the bound is re-checked above regardless.
                    unsafe { *args.add(slot as usize) = replacement };
                }
            }
            // A returned string is a fresh handle the native caller frees.
            let value = marshal::lower_result(&session.library, returned.result);
            // SAFETY: the caller guarantees `out` is one writable value.
            unsafe { *out = value };
        }
        // A trap has nowhere to go from here: unwinding out of an `extern "C"`
        // frame aborts, and the native caller has no error channel. Report and
        // exit as the native runtime's own traps do, so a trap reached through
        // native code and one reached directly look the same to a user.
        Err(trap) => fatal(&format!("runtime trap: {trap}")),
    }
}

/// Reports a condition the seam cannot return from, and exits.
///
/// Spelled the way `kira_rt_trap_div_zero` spells its own: this is reached from
/// inside native code, and which side of the boundary a trap happened on is not
/// something a user should have to notice.
fn fatal(message: &str) -> ! {
    eprintln!("kira: {message}");
    std::process::exit(1);
}

/// Reads a file, naming it on failure.
fn read(path: &Path) -> Result<Vec<u8>, HybridError> {
    std::fs::read(path).map_err(|source| HybridError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Resolves a manifest-recorded payload path against the manifest's directory.
fn resolve(base: &Path, recorded: &str) -> PathBuf {
    let path = Path::new(recorded);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

/// Asks the native half for its heap balance when a run ends, however it ends.
///
/// A guard rather than a call at the end of `run`, because a run that traps
/// leaves through `?` and a leak is exactly as interesting on that path — more
/// so, since an early return is where a release is most often skipped.
struct HeapReportAtExit<'a> {
    library: &'a NativeLibrary,
}

impl<'a> HeapReportAtExit<'a> {
    fn new(library: &'a NativeLibrary) -> Self {
        HeapReportAtExit { library }
    }
}

impl Drop for HeapReportAtExit<'_> {
    fn drop(&mut self) {
        self.library.report_heap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_relative_payload_resolves_against_the_manifest_directory() {
        assert_eq!(
            resolve(Path::new("/tmp/build"), "demo.kbc"),
            PathBuf::from("/tmp/build/demo.kbc"),
        );
    }

    #[test]
    fn an_absolute_payload_is_left_alone() {
        assert_eq!(
            resolve(Path::new("/tmp/build"), "/elsewhere/demo.kbc"),
            PathBuf::from("/elsewhere/demo.kbc"),
        );
    }

    /// No session is installed outside a run, which is what stops a stray
    /// callback from running against a null pointer.
    #[test]
    fn no_session_is_active_on_a_fresh_thread() {
        assert!(ACTIVE_SESSION.get().is_null());
    }
}
