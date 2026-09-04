//! How a module is entered from outside: the C `main` of an executable, and the
//! per-function trampolines of a hybrid library.
//!
//! The entry-bearing [`ModuleKind`](super::ModuleKind)s differ precisely here.
//! An executable is entered once by the operating system at `main`, a whole
//! native live library at its fixed runner symbol, and a hybrid library many
//! times by its host through one trampoline per `@Native` function.

use std::ffi::CStr;

use kira_ir::IrFunction;
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::LLVMValueRef;

use super::ffi::c_string;
use super::symbols::trampoline_name;
use super::{Codegen, CodegenTarget};
use crate::LlvmError;

impl Codegen<'_> {
    /// Emits the C `main` that runs the program.
    ///
    /// It calls `@Main` and exits 0, mirroring the CLI's VM path: the VM
    /// discards the entrypoint's result and reports success, so native does the
    /// same — freeing the result first when it owns a string, exactly as the VM
    /// drops it.
    pub(super) fn lower_entry_point(&mut self) -> Result<(), LlvmError> {
        self.lower_process_entry(c"main", false)
    }

    /// Emits the fixed entry symbol a whole-program native live library exports.
    pub(super) fn lower_native_live_entry_point(&mut self) -> Result<(), LlvmError> {
        let symbol = c_string(kira_runtime_abi::NATIVE_LIVE_ENTRY_SYMBOL);
        self.lower_process_entry(&symbol, true)
    }

    /// Emits a zero-argument process entry that calls `@Main` and returns a
    /// runner-friendly status code.
    fn lower_process_entry(&mut self, symbol: &CStr, exported: bool) -> Result<(), LlvmError> {
        let main_return = self
            .program
            .main_function()
            .map(|function| function.return_type)
            .ok_or(LlvmError::internal("an executable with no entrypoint"))?;
        let index = self
            .program
            .main
            .ok_or(LlvmError::internal("an executable with no entrypoint"))?;
        let entry = self.functions[index as usize]
            .ok_or(LlvmError::internal("an entrypoint with no native body"))?;
        // WebAssembly has no process main thread. Its frontend refuses
        // `@MainThread`, and its entry must therefore call the helper body
        // directly instead of importing the native thread runtime.
        let native_event_loop = matches!(&self.target, CodegenTarget::Native(_));
        let dispatcher = native_event_loop
            .then(|| self.lower_main_thread_dispatcher())
            .transpose()?
            .flatten();
        let lifecycle_resolver = native_event_loop
            .then(|| self.lower_main_thread_lifecycle_resolver())
            .transpose()?;

        // SAFETY: every value and type below belongs to this live module, and
        // the builder is positioned on a block of the function being built.
        unsafe {
            let entry_ty = LLVMFunctionType(self.types.i32, std::ptr::null_mut(), 0, 0);
            let helper = LLVMAddFunction(self.module, c"kira_main_helper".as_ptr(), entry_ty);
            let helper_block =
                LLVMAppendBasicBlockInContext(self.context, helper, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, helper_block);

            // Reference the runtime's ABI marker before anything else. The call
            // is empty and free; emitting it is what makes a runtime archive
            // built against a different `kira_rt_*` contract fail to link by
            // name, instead of resolving the old code under the new ABI and
            // corrupting memory at run time.
            self.call_runtime(self.runtime.abi_marker, &mut [], c"");
            // A native archive may serve more than one hybrid run on this
            // thread. Match the VM's per-run TaskExecutor and ChannelExecutor
            // before entering the program rather than letting handles and
            // queued work leak across entrypoints.
            self.call_runtime(self.runtime.task_reset, &mut [], c"");
            self.call_runtime(self.runtime.channel_reset, &mut [], c"");

            // Module constants are filled before `@Main` runs — each by one
            // call of its init, in the compiler's dependency order — so the
            // first read anywhere in the program finds its slot filled.
            if let Some(init) = self.lower_constants_init()? {
                let init_ty = LLVMFunctionType(self.types.void, std::ptr::null_mut(), 0, 0);
                LLVMBuildCall2(
                    self.builder,
                    init_ty,
                    init,
                    std::ptr::null_mut(),
                    0,
                    c"".as_ptr(),
                );
            }

            let name = if main_return == Type::Void {
                c"".as_ptr()
            } else {
                c"kira.main.result".as_ptr()
            };
            let result = LLVMBuildCall2(
                self.builder,
                entry.ty,
                entry.value,
                std::ptr::null_mut(),
                0,
                name,
            );
            if main_return == Type::String {
                self.call_runtime(self.runtime.str_free, &mut [result], c"");
            }
            // The constants go back before the heap is asked to balance, so a
            // clean program still reports every allocation reclaimed.
            //
            // Only where `@Main` returning is the end of the program. Under a
            // native event loop it is not: `@MainThreadLifecycle` runs as a
            // fiber the main thread keeps servicing after the helper is done —
            // `kira_rt_main_thread_run` leaves its loop on
            // `helper_result.is_some() && !fiber::active()`, so a lifecycle
            // that opened a window outlives `@Main` by the whole life of the
            // window. Releasing here freed every module constant out from under
            // it, and a read after that found a default-constructed value: a
            // catalog with no catalogs in it, a list with no items. The release
            // moves to the real entry, after the loop this helper is only one
            // participant in has finished. Same reason a consumer-entered
            // library skips it — see `constants.rs`.
            if !native_event_loop {
                self.lower_constants_release()?;
                // The native counterpart of the VM's `current == 0`: after the
                // program's last value is released and before the process is
                // gone, ask the runtime whether everything it allocated came
                // back. Silent unless `KIRA_HEAP_REPORT` is set, so an ordinary
                // run pays one `getenv` here and nothing else.
                self.call_runtime(self.runtime.heap_report, &mut [], c"");
            }
            LLVMBuildRet(self.builder, LLVMConstInt(self.types.i32, 0, 0));

            let main = LLVMAddFunction(self.module, symbol.as_ptr(), entry_ty);
            if exported && cfg!(target_env = "msvc") {
                LLVMSetDLLStorageClass(
                    main,
                    llvm_sys::LLVMDLLStorageClass::LLVMDLLExportStorageClass,
                );
            }
            let block = LLVMAppendBasicBlockInContext(self.context, main, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
            let result = if native_event_loop {
                if let Some(dispatcher) = dispatcher {
                    self.call_runtime(
                        self.runtime.main_thread_install_dispatcher,
                        &mut [dispatcher],
                        c"",
                    );
                }
                self.call_runtime(
                    self.runtime.main_thread_install_lifecycle_resolver,
                    &mut [lifecycle_resolver.ok_or(LlvmError::internal(
                        "a native entry with no lifecycle resolver",
                    ))?],
                    c"",
                );
                let mut helper_args = [helper];
                LLVMBuildCall2(
                    self.builder,
                    self.runtime.main_thread_run.ty,
                    self.runtime.main_thread_run.value,
                    helper_args.as_mut_ptr(),
                    1,
                    c"kira.main.status".as_ptr(),
                )
            } else {
                LLVMBuildCall2(
                    self.builder,
                    entry_ty,
                    helper,
                    std::ptr::null_mut(),
                    0,
                    c"kira.main.status".as_ptr(),
                )
            };
            // The lifecycle is over only once the loop above has returned, so
            // this is where a native-event-loop program's constants go back and
            // where its heap is asked to balance.
            if native_event_loop {
                self.lower_constants_release()?;
                self.call_runtime(self.runtime.heap_report, &mut [], c"");
            }
            LLVMBuildRet(self.builder, result);
        }
        Ok(())
    }

    /// Emits the function-id resolver used when a lifecycle call is serviced.
    pub(super) fn lower_main_thread_lifecycle_resolver(
        &mut self,
    ) -> Result<LLVMValueRef, LlvmError> {
        let symbol = c_string("kira_main_thread_lifecycle_resolve");
        let mut params = [self.types.i32];
        // SAFETY: both types belong to this module's live context.
        let signature = unsafe { LLVMFunctionType(self.types.ptr, params.as_mut_ptr(), 1, 0) };
        // SAFETY: this module owns the resolver and every block appended below.
        let resolver = unsafe { LLVMAddFunction(self.module, symbol.as_ptr(), signature) };
        // SAFETY: the resolver, builder, and every referenced function value
        // belong to this module's live context.
        unsafe {
            let entry = LLVMAppendBasicBlockInContext(self.context, resolver, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, entry);
            let requested = LLVMGetParam(resolver, 0);
            for index in self.program.main_thread_lifecycles.iter().copied() {
                if self.engine_of(index as usize) != kira_runtime_abi::Execution::Native {
                    continue;
                }
                let Some(target) = self.functions.get(index as usize).copied().flatten() else {
                    continue;
                };
                let selected = LLVMAppendBasicBlockInContext(
                    self.context,
                    resolver,
                    c"lifecycle.selected".as_ptr(),
                );
                let next = LLVMAppendBasicBlockInContext(
                    self.context,
                    resolver,
                    c"lifecycle.next".as_ptr(),
                );
                let matches = LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    requested,
                    LLVMConstInt(self.types.i32, u64::from(index), 0),
                    c"lifecycle.matches".as_ptr(),
                );
                LLVMBuildCondBr(self.builder, matches, selected, next);
                LLVMPositionBuilderAtEnd(self.builder, selected);
                LLVMBuildRet(self.builder, target.value);
                LLVMPositionBuilderAtEnd(self.builder, next);
            }
            LLVMBuildRet(self.builder, LLVMConstPointerNull(self.types.ptr));
        }
        Ok(resolver)
    }

    /// Emits the C-callable dispatcher the main-thread runtime uses to enter a
    /// resolved `@MainThread` target.
    ///
    /// Native targets go through the same bridge trampoline a hybrid host uses.
    /// Runtime targets in a hybrid module go through the already-installed VM
    /// invoker, which keeps the compiler unaware of the host's event-loop
    /// implementation. A module with no reachable targets still gets an empty
    /// dispatcher, because the hybrid linker forces this stable host symbol
    /// into every native image and the loader may resolve it before it knows
    /// whether this particular module uses the capability.
    pub(super) fn lower_main_thread_dispatcher(
        &mut self,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let targets: Vec<usize> = self
            .program
            .functions
            .iter()
            .enumerate()
            .filter(|(index, function)| {
                function.is_main_thread && self.reachable.get(*index).copied().unwrap_or(false)
            })
            .map(|(index, _)| index)
            .collect();
        let symbol = c_string("kira_main_thread_dispatch");
        let mut params = [
            self.types.i32,
            self.types.ptr,
            self.types.i32,
            self.types.ptr,
        ];
        // SAFETY: every parameter and result type belongs to this live LLVM
        // context, and `params` remains valid for the duration of the call.
        let signature = unsafe {
            LLVMFunctionType(self.types.void, params.as_mut_ptr(), params.len() as u32, 0)
        };
        // SAFETY: this module owns the dispatcher declaration and its context.
        let dispatcher = unsafe { LLVMAddFunction(self.module, symbol.as_ptr(), signature) };
        // SAFETY: the block and all values below belong to this live module.
        unsafe {
            let entry = LLVMAppendBasicBlockInContext(self.context, dispatcher, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, entry);
            let function = LLVMGetParam(dispatcher, 0);
            let args = LLVMGetParam(dispatcher, 1);
            let count = LLVMGetParam(dispatcher, 2);
            let out = LLVMGetParam(dispatcher, 3);
            let trampoline_ty = {
                let mut params = [self.types.ptr, self.types.i32, self.types.ptr];
                LLVMFunctionType(self.types.void, params.as_mut_ptr(), params.len() as u32, 0)
            };

            for index in targets.iter().copied() {
                let selected = LLVMAppendBasicBlockInContext(
                    self.context,
                    dispatcher,
                    c"main.thread.selected".as_ptr(),
                );
                let next = LLVMAppendBasicBlockInContext(
                    self.context,
                    dispatcher,
                    c"main.thread.next".as_ptr(),
                );
                let expected = LLVMConstInt(self.types.i32, index as u64, 0);
                let matches = LLVMBuildICmp(
                    self.builder,
                    llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                    function,
                    expected,
                    c"main.thread.target".as_ptr(),
                );
                LLVMBuildCondBr(self.builder, matches, selected, next);
                LLVMPositionBuilderAtEnd(self.builder, selected);
                if self.engine_of(index) == kira_runtime_abi::Execution::Native {
                    let name = c_string(&trampoline_name(index));
                    let trampoline = LLVMGetNamedFunction(self.module, name.as_ptr());
                    let trampoline = if trampoline.is_null() {
                        LLVMAddFunction(self.module, name.as_ptr(), trampoline_ty)
                    } else {
                        trampoline
                    };
                    let mut call_args = [args, count, out];
                    LLVMBuildCall2(
                        self.builder,
                        trampoline_ty,
                        trampoline,
                        call_args.as_mut_ptr(),
                        call_args.len() as u32,
                        c"".as_ptr(),
                    );
                } else {
                    let mut call_args = [
                        LLVMConstInt(self.types.i32, index as u64, 0),
                        args,
                        count,
                        out,
                    ];
                    self.call_runtime(self.runtime.call_runtime, &mut call_args, c"");
                }
                LLVMBuildRetVoid(self.builder);
                LLVMPositionBuilderAtEnd(self.builder, next);
            }
            LLVMBuildRetVoid(self.builder);
        }
        Ok(Some(dispatcher))
    }

    /// The address of `args[slot]`, one `BridgeValue` into the argument array.
    ///
    /// # Safety
    ///
    /// `args` must point at an array of at least `slot + 1` `BridgeValue`s, and
    /// the builder must be positioned on a live block.
    unsafe fn bridge_slot(&self, args: LLVMValueRef, slot: u32) -> LLVMValueRef {
        // SAFETY: the type belongs to this live module's context.
        let mut offset = [unsafe { LLVMConstInt(self.types.i32, u64::from(slot), 0) }];
        // SAFETY: the caller vouches for `args`' extent and for the builder
        // being positioned, which is the whole of this function's contract.
        unsafe {
            LLVMBuildInBoundsGEP2(
                self.builder,
                self.types.bridge_value,
                args,
                offset.as_mut_ptr(),
                1,
                c"arg.slot".as_ptr(),
            )
        }
    }

    /// Emits the trampoline the host calls to reach native function `index`.
    ///
    /// ```text
    /// void kira_native_fn_<id>(BridgeValue *args, u32 count, BridgeValue *out)
    /// ```
    ///
    /// `args` is written as well as read: a parameter the callee writes through
    /// has its final value packed back into the slot it arrived in, which is how
    /// a `borrow mut` crosses a seam whose two sides share no heap.
    ///
    /// One C-ABI shape for every Kira signature, so the host can call any native
    /// function through one function-pointer type rather than needing a
    /// per-signature thunk. The trampoline unpacks each argument to the type the
    /// manifest promised, calls the real body, and packs the result back.
    ///
    /// `count` is not checked against the signature: the host builds the call
    /// from the same manifest this was generated from, so a mismatch is a broken
    /// artifact rather than a runtime condition — and the manifest's decoder is
    /// where artifacts are validated.
    pub(super) fn lower_trampoline(
        &mut self,
        index: usize,
        function: &IrFunction,
    ) -> Result<(), LlvmError> {
        let target = self.functions[index].ok_or(LlvmError::internal(
            "a trampoline to a function with no body",
        ))?;
        let symbol = c_string(&trampoline_name(index));
        let types = self.types;

        // SAFETY: every type and value below belongs to this live module, and
        // the builder is positioned on the trampoline's own block before any
        // instruction is built.
        unsafe {
            let mut params = [types.ptr, types.i32, types.ptr];
            let signature =
                LLVMFunctionType(types.void, params.as_mut_ptr(), params.len() as u32, 0);
            let trampoline = LLVMAddFunction(self.module, symbol.as_ptr(), signature);
            // The one thing a trampoline is for is being found by name from
            // outside, and on PE/COFF that does not follow from defining it.
            // ELF and Mach-O export a definition by default, so a hybrid
            // library built on those hosts needed nothing here; a DLL exports
            // only what it was told to, and the host's own check caught the
            // result exactly — `app.dll` "does not export `kira_native_fn_0`".
            //
            // Marked at emission rather than with a `/EXPORT:` flag per symbol
            // because the trampolines are ours: the count and the names are
            // decided right here, and a link-time list would be a second place
            // to keep them in step.
            if cfg!(target_env = "msvc") {
                LLVMSetDLLStorageClass(
                    trampoline,
                    llvm_sys::LLVMDLLStorageClass::LLVMDLLExportStorageClass,
                );
            }
            let block = LLVMAppendBasicBlockInContext(self.context, trampoline, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);

            let args = LLVMGetParam(trampoline, 0);
            let out = LLVMGetParam(trampoline, 2);

            let mut lowered = Vec::with_capacity(function.param_count as usize);
            // The parameters the callee writes through, and the storage it
            // writes into. Collected on the way in so the way out does not have
            // to ask the signature a second time.
            let mut written_through = Vec::new();
            for slot in 0..function.param_count {
                let ty = function
                    .param_type(slot)
                    .ok_or(LlvmError::internal("a parameter with no type"))?;
                let element = self.bridge_slot(args, slot);
                let value = self.read_bridge_payload(element, ty)?;
                // A written-through parameter arrives as a value like any
                // other — the two engines share no heap, so what crossed is a
                // copy — but the callee's signature takes a pointer, because
                // within this half a write through one lands in the caller's
                // storage. Here the caller is the other engine, so the storage
                // is this frame's: the callee mutates it, and the final value
                // goes back the way the result does.
                if self.param_is_pointer(function, slot) {
                    let storage = LLVMBuildAlloca(
                        self.builder,
                        self.llvm_type(ty)?,
                        c"arg.mut.storage".as_ptr(),
                    );
                    LLVMBuildStore(self.builder, value, storage);
                    lowered.push(storage);
                    written_through.push((slot, storage, ty));
                } else {
                    lowered.push(value);
                }
            }

            let returns_value = function.return_type != Type::Void;
            let name = if returns_value { c"result" } else { c"" };
            let result = LLVMBuildCall2(
                self.builder,
                target.ty,
                target.value,
                lowered.as_mut_ptr(),
                lowered.len() as u32,
                name.as_ptr(),
            );
            // Each written-through parameter's final value replaces the argument
            // that arrived in its slot. That argument's own tree was consumed by
            // the decode above, so the slot holds nothing to free — writing a
            // fresh tree into it transfers the new value to the caller, exactly
            // as the result is transferred.
            for (slot, storage, ty) in written_through {
                let final_value = LLVMBuildLoad2(
                    self.builder,
                    self.llvm_type(ty)?,
                    storage,
                    c"arg.mut.final".as_ptr(),
                );
                let element = self.bridge_slot(args, slot);
                self.write_bridge_value(element, final_value, ty)?;
            }
            self.write_bridge_value(out, result, function.return_type)?;
            LLVMBuildRetVoid(self.builder);
        }
        Ok(())
    }
}
