//! How a module is entered from outside: the C `main` of an executable, and the
//! per-function trampolines of a hybrid library.
//!
//! The two [`ModuleKind`](super::ModuleKind)s differ precisely here. An
//! executable is entered once, by the operating system, at `main`. A hybrid
//! library is entered many times, by its host, one call per crossing — so it
//! exports a fixed-shape trampoline per `@Native` function instead of a `main`.

use kira_ir::IrFunction;
use kira_semantics_model::Type;
use llvm_sys::core::*;

use super::Codegen;
use super::ffi::c_string;
use super::symbols::trampoline_name;
use crate::LlvmError;

impl Codegen<'_> {
    /// Emits the C `main` that runs the program.
    ///
    /// It calls `@Main` and exits 0, mirroring the CLI's VM path: the VM
    /// discards the entrypoint's result and reports success, so native does the
    /// same — freeing the result first when it owns a string, exactly as the VM
    /// drops it.
    pub(super) fn lower_entry_point(&mut self) -> Result<(), LlvmError> {
        let main_function = self
            .program
            .main_function()
            .ok_or(LlvmError::Unsupported("an executable with no entrypoint"))?;
        let index = self
            .program
            .main
            .ok_or(LlvmError::Unsupported("an executable with no entrypoint"))?;
        let entry = self.functions[index as usize]
            .ok_or(LlvmError::Unsupported("an entrypoint with no native body"))?;

        // SAFETY: every value and type below belongs to this live module, and
        // the builder is positioned on a block of the function being built.
        unsafe {
            let main_ty = LLVMFunctionType(self.types.i32, std::ptr::null_mut(), 0, 0);
            let main = LLVMAddFunction(self.module, c"main".as_ptr(), main_ty);
            let block = LLVMAppendBasicBlockInContext(self.context, main, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);

            // Reference the runtime's ABI marker before anything else. The call
            // is empty and free; emitting it is what makes a runtime archive
            // built against a different `kira_rt_*` contract fail to link by
            // name, instead of resolving the old code under the new ABI and
            // corrupting memory at run time.
            self.call_runtime(self.runtime.abi_marker, &mut [], c"");

            let name = if main_function.return_type == Type::Void {
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
            if main_function.return_type == Type::String {
                self.call_runtime(self.runtime.str_free, &mut [result], c"");
            }
            LLVMBuildRet(self.builder, LLVMConstInt(self.types.i32, 0, 0));
        }
        Ok(())
    }

    /// Emits the trampoline the host calls to reach native function `index`.
    ///
    /// ```text
    /// void kira_native_fn_<id>(const BridgeValue *args, u32 count, BridgeValue *out)
    /// ```
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
        let target = self.functions[index].ok_or(LlvmError::Unsupported(
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
            let block = LLVMAppendBasicBlockInContext(self.context, trampoline, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);

            let args = LLVMGetParam(trampoline, 0);
            let out = LLVMGetParam(trampoline, 2);

            let mut lowered = Vec::with_capacity(function.param_count as usize);
            for slot in 0..function.param_count {
                let ty = function
                    .param_type(slot)
                    .ok_or(LlvmError::Unsupported("a parameter with no type"))?;
                let mut offset = [LLVMConstInt(types.i32, u64::from(slot), 0)];
                let element = LLVMBuildInBoundsGEP2(
                    self.builder,
                    types.bridge_value,
                    args,
                    offset.as_mut_ptr(),
                    1,
                    c"arg.slot".as_ptr(),
                );
                lowered.push(self.read_bridge_payload(element, ty)?);
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
            self.write_bridge_value(out, result, function.return_type)?;
            LLVMBuildRetVoid(self.builder);
        }
        Ok(())
    }
}
