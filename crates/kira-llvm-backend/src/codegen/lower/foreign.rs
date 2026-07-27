//! Foreign call lowering: marshalling a Kira call site to a generated adapter.
//!
//! The mirror of the VM's `CALL_FOREIGN` interpreter path. Arguments are packed
//! into a stack array of `BridgeValue`s tagged for the import's exact-width
//! signature, the generated adapter is called directly, and the checked result
//! is read back as a Kira value. A non-success status is a runtime trap, exactly
//! as the VM surfaces a `ForeignCallError` — there is no value to hand back.

use kira_ir::IrExprId;
use kira_runtime_abi::{BridgeValueTag, ForeignPointerWidth, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to foreign import `index`.
    ///
    /// `result_ty` is the Kira type of the call expression, which an aggregate
    /// result needs and a scalar one ignores: rebuilding a struct out of C bytes
    /// takes the struct's own LLVM type, and the signature only names the
    /// aggregate's table row.
    pub(super) fn lower_foreign_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
        result_ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let idx = index as usize;
        let import = self.codegen.program.foreign_imports[idx].import.clone();
        let signature = import.signature();
        let params: Vec<ForeignTypeSpec> = signature.parameters().to_vec();
        let result_spec = signature.result();
        let adapter = *self
            .codegen
            .foreign_adapters
            .get(idx)
            .ok_or(LlvmError::Unsupported(
                "a call to an undeclared foreign adapter",
            ))?;

        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len());
        for &argument in args {
            let ty = self.type_of(argument);
            values.push((self.lower_expr(argument)?, ty));
        }

        let types = self.codegen.types;
        let builder = self.codegen.builder;

        // An aggregate argument's C-layout bytes live in a buffer this frame
        // owns for the length of the call, and the bridge slot carries a pointer
        // to it — the same contract the VM host follows, so both sides hand the
        // adapter the identical thing.
        let mut aggregate_buffers = Vec::new();
        for ((value, ty), spec) in values.iter().copied().zip(params.iter().copied()) {
            let Some(id) = spec.aggregate() else {
                continue;
            };
            aggregate_buffers.push(self.write_aggregate_buffer(id, value, ty)?);
        }

        // The stack this call's buffers take is given back the moment it
        // returns. Reserving without restoring leaks a frame's worth per
        // iteration when the call sits in a loop; hoisting to the entry block
        // instead would reserve *every* call site's buffers on entry, which on
        // a large dispatch function is a quarter-megabyte frame.
        let saved = self.call(self.codegen.runtime.stack_save, &mut [], c"stack.save");
        // SAFETY: every type and value belongs to this live module and the
        // builder is on a live block; `argv` is sized to hold exactly the
        // arguments written into it, and `out` addresses one bridge value.
        let (out, result_buffer, status) = unsafe {
            let count = LLVMConstInt(types.i64, values.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"foreign.args".as_ptr());
            let mut buffers = aggregate_buffers.into_iter();
            for (slot, ((value, ty), spec)) in values
                .iter()
                .copied()
                .zip(params.iter().copied())
                .enumerate()
            {
                let element = self.codegen.bridge_element_ptr(argv, slot as u64);
                match spec {
                    ForeignTypeSpec::Aggregate(_) => {
                        let buffer = buffers.next().ok_or(LlvmError::Unsupported(
                            "an aggregate argument with no marshalling buffer",
                        ))?;
                        self.codegen.write_bridge_pointer(
                            element,
                            buffer,
                            BridgeValueTag::AGGREGATE,
                        );
                    }
                    ForeignTypeSpec::Scalar(ft) => {
                        self.codegen.write_foreign_arg(element, value, ty, ft)?;
                    }
                }
            }
            let out = LLVMBuildAlloca(builder, types.bridge_value, c"foreign.out".as_ptr());
            // The caller presents the result buffer: the adapter writes into it
            // and never allocates, so nothing crosses ownership.
            let result_buffer = match result_spec.aggregate() {
                Some(id) => {
                    let buffer = self.aggregate_alloca(id)?;
                    self.codegen
                        .write_bridge_pointer(out, buffer, BridgeValueTag::AGGREGATE);
                    Some((id, buffer))
                }
                None => None,
            };
            let mut call_args = [argv, LLVMConstInt(types.i32, values.len() as u64, 0), out];
            let status = self
                .codegen
                .call_runtime(adapter, &mut call_args, c"foreign.status");
            (out, result_buffer, status)
        };

        // A non-success status is a runtime trap (an interior NUL, say): there is
        // no value to hand back, so native code reports it and exits, mirroring
        // the VM surfacing a `ForeignCallError`.
        let function = self.current_function();
        let ok = self.append_block(function, c"foreign.ok");
        let fail = self.append_block(function, c"foreign.fail");
        // SAFETY: the builder is on the call's block; `status` is the adapter's
        // `i32` return.
        unsafe {
            let zero = LLVMConstInt(types.i32, 0, 0);
            let failed = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntNE,
                status,
                zero,
                c"foreign.failed".as_ptr(),
            );
            LLVMBuildCondBr(builder, failed, fail, ok);
        }
        self.position_at(fail);
        self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
        // SAFETY: `kira_rt_trap_foreign` never returns; the block is terminated.
        unsafe { LLVMBuildUnreachable(builder) };
        self.position_at(ok);

        // Read the result *before* giving the stack back: the buffers holding
        // it are the ones about to be released.
        let value = match result_buffer {
            Some((id, buffer)) => self.read_aggregate_buffer(id, buffer, result_ty)?,
            None => {
                let scalar = crate::codegen::foreign_scalar::scalar_of(result_spec)?;
                self.codegen.read_foreign_result(out, scalar)?
            }
        };
        self.call(self.codegen.runtime.stack_restore, &mut [saved], c"");
        Ok(value)
    }

    /// The layout width the host this build targets uses.
    ///
    /// A native build runs on the machine it was built for, so the pointer width
    /// the shim was compiled with and the width used here are the same one.
    pub(super) fn pointer_width(&self) -> ForeignPointerWidth {
        self.codegen.pointer_width
    }

    /// An alloca sized and aligned for one aggregate's C layout.
    pub(super) fn aggregate_alloca(
        &mut self,
        id: kira_runtime_abi::ForeignAggregateId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let layout = self
            .codegen
            .program
            .foreign_aggregates
            .layout_of(id, self.pointer_width())
            .map_err(|_| LlvmError::Unsupported("an aggregate with no computable C layout"))?;
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        // SAFETY: the builder is on a live block and `types.i8` belongs to this
        // module's context; the array length is the aggregate's own `sizeof`.
        Ok(unsafe {
            let count = LLVMConstInt(types.i64, u64::from(layout.size), 0);
            let buffer = LLVMBuildArrayAlloca(builder, types.i8, count, c"foreign.agg".as_ptr());
            // C alignment, not the `i8` array's: the shim dereferences this as
            // the struct type, and an under-aligned load is undefined.
            LLVMSetAlignment(buffer, layout.align);
            buffer
        })
    }
}
