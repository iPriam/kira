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
                let result = match observer {
                    Some(observer) => program.run_with_debug(&mut host, observer),
                    None => program.run(&mut host),
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
        match value {
            NativeStateValue::Cell(cell) => {
                let handle = cell.handle();
                if let Some(vm_handle) = self.library.vm_cell_proxy_handle(handle) {
                    *cell = kira_runtime_abi::NativeCell::from_vm(vm_handle, |_| {});
                }
            }
            NativeStateValue::Struct(fields) | NativeStateValue::Array(fields) => {
                for field in Arc::make_mut(fields) {
                    self.rewrite_vm_cell_proxies(field);
                }
            }
            NativeStateValue::Enum { payload, .. } => {
                if let Some(payload) = payload {
                    self.rewrite_vm_cell_proxies(Arc::make_mut(payload));
                }
            }
            NativeStateValue::Any { payload, .. } => {
                self.rewrite_vm_cell_proxies(Arc::make_mut(payload));
            }
            NativeStateValue::Int(_)
            | NativeStateValue::Float(_)
            | NativeStateValue::Bool(_)
            | NativeStateValue::String(_)
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
        let signature = self
            .current_program()
            .0
            .module()
            .foreign_callbacks
            .get(index)
            .map(|callback| callback.signature().clone())
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
        let runtime = self
            .libffi
            .as_ref()
            .ok_or(ForeignCallError::NoForeignHost)?;
        let function_id = self
            .current_program()
            .0
            .module()
            .foreign_callbacks
            .get(index)
            .map(|callback| callback.function())
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
