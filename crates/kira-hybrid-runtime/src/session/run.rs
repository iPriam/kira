//! Running a loaded session, and the seam it holds open while it runs.
//!
//! Split from the loading half because the two answer different questions: that
//! one is about a manifest becoming a `Session`, this one is about what a
//! running program may reach — the native half, a foreign library, the kernel.
//!
//! These are further inherent methods on [`Session`](super::Session); one type's
//! impl blocks may live in several modules of a crate, so the split costs a
//! caller nothing.

use super::callback::ffi_callback_entry;
use super::*;

use kira_runtime_abi::MainThreadError;
use kira_vm_runtime::MainThreadRunner;
use std::sync::OnceLock;

static NATIVE_ENTRY_CONTEXT: OnceLock<Mutex<Option<(usize, u32)>>> = OnceLock::new();

fn native_entry_context() -> &'static Mutex<Option<(usize, u32)>> {
    NATIVE_ENTRY_CONTEXT.get_or_init(|| Mutex::new(None))
}

/// The deepest native-state aggregate the seam will walk.
///
/// Chosen far above any shape a real program returns and far below what would
/// exhaust a thread stack, so it only ever fires on a value that was never going
/// to be readable.
const MAX_NATIVE_STATE_DEPTH: usize = 128;

impl Session {
    /// Runs the program's entrypoint to completion.
    ///
    /// The entrypoint may live on either engine — `@Main` is annotatable like
    /// anything else — so this dispatches on the engine the manifest records
    /// rather than assuming the bytecode half starts every program.
    pub fn run(&self) -> Result<(), HybridError> {
        self.run_inner(None)
    }

    /// Runs the VM half with an instruction-level debugger attached.
    ///
    /// If the manifest's entrypoint is native, the observer has no VM frame to
    /// inspect; that native body remains available to LLDB through the DWARF
    /// symbols emitted for the hybrid library. The CLI's `debug --lldb` mode
    /// hosts this same session inside a real LLDB child process.
    pub fn run_with_debug(&self, observer: &mut dyn VmDebugObserver) -> Result<(), HybridError> {
        self.run_inner(Some(observer))
    }

    fn run_inner(&self, observer: Option<&mut dyn VmDebugObserver>) -> Result<(), HybridError> {
        let _active = ActiveSession::install(self);
        // The native half of a hybrid program is a shared library, so it has no
        // emitted `main` to report its heap balance from. The host asks on its
        // behalf once the run is over — see `HeapReportAtExit`.
        let _heap_report = HeapReportAtExit::new(&self.library);
        let _task_scope = TaskScopeAtExit::new(&self.library);
        let entry = self
            .manifest
            .entry_function()
            .ok_or(HybridError::NoEntrypoint)?;
        let (program, _, _) = self.current_program();

        match entry.execution {
            Execution::Native => run_native_entry(self, entry.id, &entry.name),
            _ => {
                self.library.install_main_thread_dispatcher();
                self.library.reset_main_thread_lifecycles();
                let mut host = Host { session: self };
                let result = match observer {
                    Some(observer) => {
                        let runner = HybridMainThreadRunner { session: self };
                        kira_vm_runtime::execute_with_main_thread_using_debug(
                            program.module(),
                            &mut host,
                            runner,
                            observer,
                        )
                    }
                    None => {
                        let runner = HybridMainThreadRunner { session: self };
                        kira_vm_runtime::execute_with_main_thread_using(
                            program.module(),
                            &mut host,
                            runner,
                        )
                    }
                };
                result.map(|_| ()).map_err(HybridError::Trap)
            }
        }
    }

    /// Calls one native function: the runtime-to-native direction.
    pub(super) fn call_native(
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
        let mut result = unsafe { marshal::lift_result(&self.library, out) }
            .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        self.rewrite_vm_cell_proxies_result(&mut result);

        // SAFETY: `lowered` is the array that call just wrote through, and no
        // written-through slot has been lifted yet.
        let mut writebacks =
            unsafe { marshal::lift_writebacks(&self.library, function_id, &lowered) }
                .map_err(|_| NativeCallError::MalformedResult(function_id))?;
        for (_, value) in &mut writebacks {
            self.rewrite_vm_cell_proxies_result(value);
        }
        Ok(NativeReturn { result, writebacks })
    }

    pub(super) fn rewrite_vm_cell_proxies_result(&self, result: &mut NativeResult) {
        if let NativeResult::Aggregate(value) = result {
            self.rewrite_vm_cell_proxies(value);
        }
    }

    pub(super) fn rewrite_vm_cell_proxies(&self, value: &mut NativeStateValue) {
        self.rewrite_vm_cell_proxies_to_depth(value, 0);
    }

    /// The walk itself, carrying how deep it already is.
    ///
    /// `NativeStateValue` nests without bound and this walk is recursive, so a
    /// native half that hands back a deeply nested aggregate — or builds a
    /// cyclic one through `Arc` — would run the session thread out of stack
    /// rather than report anything. Neither `ForeignAggregates` nor the
    /// descriptor limits bound tree *depth*, so the bound lives here.
    ///
    /// Exceeding it is fatal rather than a silent stop: leaving the remainder
    /// unrewritten would hand the VM a native cell proxy it cannot resolve,
    /// which fails later and somewhere else.
    fn rewrite_vm_cell_proxies_to_depth(&self, value: &mut NativeStateValue, depth: usize) {
        if depth >= MAX_NATIVE_STATE_DEPTH {
            fatal(&format!(
                "native code returned a value nested deeper than {MAX_NATIVE_STATE_DEPTH} levels; \
                 the hybrid seam cannot walk it without exhausting the stack"
            ));
        }
        let depth = depth + 1;
        match value {
            NativeStateValue::Cell(cell) => {
                let handle = cell.handle();
                if let Some(vm_handle) = self.library.vm_cell_proxy_handle(handle) {
                    *cell = kira_runtime_abi::NativeCell::from_vm(vm_handle, |_| {});
                }
            }
            NativeStateValue::Struct(fields) | NativeStateValue::Array(fields) => {
                for field in Arc::make_mut(fields) {
                    self.rewrite_vm_cell_proxies_to_depth(field, depth);
                }
            }
            NativeStateValue::Enum { payload, .. } => {
                if let Some(payload) = payload {
                    self.rewrite_vm_cell_proxies_to_depth(Arc::make_mut(payload), depth);
                }
            }
            NativeStateValue::Any { payload, .. } => {
                self.rewrite_vm_cell_proxies_to_depth(Arc::make_mut(payload), depth);
            }
            NativeStateValue::Int(_)
            | NativeStateValue::Float(_)
            | NativeStateValue::Bool(_)
            | NativeStateValue::String(_)
            | NativeStateValue::CBlock(_)
            | NativeStateValue::RawPtr(_) => {}
        }
    }

    /// The system call and signature of import `foreign_id`, when it names one.
    ///
    /// Read off the manifest's own ABI byte, which has always travelled with the
    /// import: the row carries no library path for a system call, so without
    /// this the bytecode half found no binding and reported a missing library
    /// for a call that names none.
    pub(super) fn syscall_binding(
        &self,
        foreign_id: u32,
    ) -> Option<(LinuxSyscall, ForeignSignature)> {
        let import = self.manifest.foreign.get(foreign_id as usize)?;
        if import.abi.binds_a_library_symbol() {
            return None;
        }
        Some((
            LinuxSyscall::parse(&import.symbol)?,
            import.signature.clone(),
        ))
    }

    /// Calls one foreign symbol through the bundled Libffi runtime.
    pub(super) fn call_foreign(
        &self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        let import = self
            .manifest
            .foreign
            .get(foreign_id as usize)
            .ok_or(ForeignCallError::NoForeignHost)?;
        let Some(path) = self
            .foreign_paths
            .get(foreign_id as usize)
            .and_then(Option::as_deref)
        else {
            return Err(ForeignCallError::NoForeignHost);
        };
        let library = self
            .foreign_libraries
            .iter()
            .find(|library| library.path() == path)
            .ok_or(ForeignCallError::NoForeignHost)?;
        // A data symbol is bound and then nothing is invoked: the answer is
        // where the object is. The lookup below is the one a call would do, and
        // what is skipped is the call -- invoking a data symbol executes the
        // object's first bytes, which is a fault on a page that is not code.
        if import.abi.answers_an_address() {
            return library
                .symbol_address(&import.symbol)
                .map(|address| ForeignResult::RawPtr(address as u64))
                .map_err(|_| ForeignCallError::NoForeignHost);
        }
        // SAFETY: the binding and aggregate table came from the validated
        // manifest, and the library owns the exported symbol address.
        unsafe { library.call(&import.symbol, &import.signature, args) }.map_err(
            |error| match error {
                kira_dynamic_ffi::ForeignLibraryError::Call(call) => call,
                _ => ForeignCallError::NoForeignHost,
            },
        )
    }

    /// Returns the executable address of a lazily prepared Libffi callback.
    pub(super) fn callback_address(&self, callback_id: u32) -> Result<u64, ForeignCallError> {
        let index = callback_id as usize;
        // One read of the module, not two. Each `current_program` takes the lock
        // afresh, so reading the signature and the function id separately can
        // straddle a VM reload and pair a signature from one module with an id
        // from the next — a callback prepared to marshal one shape and dispatch
        // to a function expecting another.
        let (program, _, _) = self.current_program();
        let callback = program
            .module()
            .foreign_callbacks
            .get(index)
            .ok_or(ForeignCallError::NoForeignHost)?;
        let signature = callback.signature().clone();
        let function_id = callback.function();
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
        let runtime = self
            .libffi
            .as_ref()
            .ok_or(ForeignCallError::NoForeignHost)?;
        let context = Box::pin(CallbackContext {
            function_id,
            signature,
        });
        let user_data = (&*context as *const CallbackContext).cast_mut().cast();
        // SAFETY: the context is retained until the closure is dropped, and the
        // callback entry only reads the validated signature while the closure
        // is alive. The registry lock is intentionally held only while libffi
        // prepares and installs the closure; preparation cannot invoke the
        // callback, so no foreign call can re-enter this mutex.
        let closure = unsafe {
            FfiClosure::new(
                runtime,
                &context.signature,
                &self.manifest.foreign_aggregates,
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

/// Runs a native entry through the native image's helper-thread/main-thread
/// runtime.
fn run_native_entry(session: &Session, function: u32, name: &str) -> Result<(), HybridError> {
    let context = native_entry_context();
    {
        let mut slot = context.lock().unwrap_or_else(|held| held.into_inner());
        if slot.is_some() {
            return Err(HybridError::Mismatch(
                "another native hybrid entry is already running".to_owned(),
            ));
        }
        *slot = Some((ptr::from_ref(session) as usize, function));
    }
    // The entrypoint always runs on the application thread: a
    // `@MainThreadLifecycle` function owns the main thread separately, so the
    // dispatcher is installed either way.
    session.library.install_main_thread_dispatcher();
    let code = session.library.run_main_thread(native_entry_helper);
    *context.lock().unwrap_or_else(|held| held.into_inner()) = None;
    if code == 0 {
        Ok(())
    } else {
        Err(HybridError::Mismatch(format!(
            "native entrypoint `{name}` returned status {code}"
        )))
    }
}

/// The no-argument helper the native runtime invokes on its worker thread.
extern "C" fn native_entry_helper() -> i32 {
    let Some((session, function)) = native_entry_context()
        .lock()
        .unwrap_or_else(|held| held.into_inner())
        .as_ref()
        .copied()
    else {
        fatal("native hybrid entry started without a session context");
    };
    // SAFETY: `run_native_entry` stores this pointer only while it owns the
    // active `Session` borrow, and the native runtime joins this helper before
    // that function clears the context.
    let session = unsafe { &*(session as *const Session) };
    let _active = ActiveSession::bind(session);
    let Some(trampoline) = session.library.trampoline(function) else {
        fatal(&format!(
            "the native entrypoint {function} has no bound trampoline"
        ));
    };
    // SAFETY: the trampoline is the loaded image's generated entry and the
    // manifest validated that the entry has no parameters.
    let out = unsafe { session.library.call(trampoline, &mut []) };
    // SAFETY: the output belongs to this native call and any owned string or
    // value node is consumed by `lift_result`.
    if let Err(error) = unsafe { marshal::lift_result(&session.library, out) } {
        fatal(&format!(
            "native entry returned an unreadable value: {error}"
        ));
    }
    0
}

/// Dispatches a main-thread request to either the native half or the current
/// VM half of a hybrid session.
struct HybridMainThreadRunner<'a> {
    session: &'a Session,
}

impl MainThreadRunner for HybridMainThreadRunner<'_> {
    fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function: u32,
        args: &[NativeStateValue],
    ) -> Result<Option<NativeStateValue>, MainThreadError> {
        let entry = self
            .session
            .manifest
            .functions
            .get(function as usize)
            .ok_or(MainThreadError::UnknownFunction(function))?;
        match entry.execution {
            Execution::Runtime => {
                let (program, _, _) = self.session.current_program();
                program
                    .call_state(host, function, args)
                    .map_err(|error| MainThreadError::Function(error.to_string()))
            }
            Execution::Native => {
                let owned = args
                    .iter()
                    .map(owned_main_thread_arg)
                    .collect::<Result<Vec<_>, _>>()?;
                let borrowed: Vec<NativeArg<'_>> = owned.iter().map(OwnedArg::borrow).collect();
                let _active = ActiveSession::bind(self.session);
                let returned = self
                    .session
                    .call_native(function, &borrowed)
                    .map_err(|error| MainThreadError::Function(error.to_string()))?;
                native_result_state(returned.result)
            }
            Execution::Inherited => Err(MainThreadError::Function(
                "hybrid manifest left a function's execution engine inherited".to_owned(),
            )),
        }
    }

    fn start_lifecycle(&self, function: u32) -> Result<bool, MainThreadError> {
        let entry = self
            .session
            .manifest
            .functions
            .get(function as usize)
            .ok_or(MainThreadError::UnknownFunction(function))?;
        if entry.execution != Execution::Native {
            return Ok(false);
        }
        if self.session.library.start_main_thread_lifecycle(function) {
            Ok(true)
        } else {
            Err(MainThreadError::UnknownFunction(function))
        }
    }

    fn pump_lifecycles(&self, budget: u64) -> Result<bool, MainThreadError> {
        Ok(self.session.library.pump_main_thread_lifecycles(budget))
    }

    fn reset_lifecycles(&self) {
        self.session.library.reset_main_thread_lifecycles();
    }
}

/// Converts one owned state tree into the VM/native argument vocabulary.
fn owned_main_thread_arg(value: &NativeStateValue) -> Result<OwnedArg, MainThreadError> {
    Ok(match value {
        NativeStateValue::Int(value) => OwnedArg::Int(*value),
        NativeStateValue::Float(value) => OwnedArg::Float(*value),
        NativeStateValue::Bool(value) => OwnedArg::Bool(*value),
        NativeStateValue::String(value) => OwnedArg::Str(value.clone()),
        NativeStateValue::RawPtr(value) => OwnedArg::RawPtr(*value),
        NativeStateValue::Enum { tag, payload: None } => OwnedArg::Enum(i64::from(*tag)),
        NativeStateValue::Struct(_)
        | NativeStateValue::Array(_)
        | NativeStateValue::Enum {
            payload: Some(_), ..
        }
        | NativeStateValue::Any { .. }
        | NativeStateValue::CBlock(_) => OwnedArg::Aggregate(value.clone()),
        NativeStateValue::Cell(_) => {
            return Err(MainThreadError::Function(
                "a captured cell cannot cross the main-thread boundary".to_owned(),
            ));
        }
    })
}

/// Converts a native target's owned result into the main-thread runner tree.
fn native_result_state(value: NativeResult) -> Result<Option<NativeStateValue>, MainThreadError> {
    Ok(match value {
        NativeResult::Void => None,
        NativeResult::Int(value) => Some(NativeStateValue::Int(value)),
        NativeResult::Float(value) => Some(NativeStateValue::Float(value)),
        NativeResult::Bool(value) => Some(NativeStateValue::Bool(value)),
        NativeResult::Str(value) => Some(NativeStateValue::String(value)),
        NativeResult::RawPtr(value) => Some(NativeStateValue::RawPtr(value)),
        NativeResult::Enum(value) => Some(NativeStateValue::enum_of(
            u32::try_from(value).map_err(|_| {
                MainThreadError::Function("native enum tag is too large".to_owned())
            })?,
            None,
        )),
        NativeResult::Aggregate(value) => Some(value),
        NativeResult::Handle(_) => {
            return Err(MainThreadError::Function(
                "an opaque native handle cannot cross the main-thread runner".to_owned(),
            ));
        }
    })
}

/// Gives every hybrid run a fresh native task table, then clears it again.
///
/// The VM constructs its `TaskExecutor` inside each `Vm`; native storage is
/// thread-local because the C ABI has no context argument, so the session must
/// provide the equivalent lifetime explicitly.
struct TaskScopeAtExit<'a> {
    library: &'a NativeLibrary,
}

impl<'a> TaskScopeAtExit<'a> {
    fn new(library: &'a NativeLibrary) -> Self {
        library.reset_tasks();
        TaskScopeAtExit { library }
    }
}

impl Drop for TaskScopeAtExit<'_> {
    fn drop(&mut self) {
        self.library.reset_tasks();
    }
}
