//! Lowering for the file-system intrinsics.
//!
//! Each operation is one call to its `kira_rt_fs_*` helper. The helper consumes
//! the handles it is given — the native mirror of the VM dropping the operands
//! it popped — so nothing is freed here.

use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::{LLVMBuildICmp, LLVMConstInt};
use llvm_sys::prelude::LLVMValueRef;

use kira_ir::ir::IrExprId;
use kira_runtime_abi::FileSystemOp;
use kira_semantics_model::Type;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers one file-system operation to its runtime call.
    pub(in crate::codegen) fn lower_file_system(
        &mut self,
        op: FileSystemOp,
        args: &[IrExprId],
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len() + 1);
        for &arg in args {
            values.push(self.lower_expr(arg)?);
        }
        // An operation that builds or walks an array needs the element stride
        // the target gives that element type — the same number `array_new` is
        // handed everywhere else, so the runtime touches the slots generated
        // code does.
        if let Some(element) = self.file_system_element(op, args, ty) {
            values.push(self.codegen.abi_size(element)?);
        }

        let callee = self.codegen.runtime.file_system[usize::from(op.as_byte())];
        let raw = self.call(callee, &mut values, c"fs");
        Ok(match self.file_system_result_kind(op) {
            ResultKind::Flag => self.byte_to_bool(raw),
            ResultKind::Value => raw,
        })
    }

    /// The element type whose stride the helper needs, for the operations that
    /// build or read an array.
    ///
    /// Taken from the array the operation actually touches — its result for the
    /// two that build one, its second argument for the one that reads one — so
    /// the stride is the target's answer for `[U8]` / `[String]` rather than a
    /// width assumed here.
    fn file_system_element(&self, op: FileSystemOp, args: &[IrExprId], ty: Type) -> Option<Type> {
        let array = match op {
            FileSystemOp::ReadRange | FileSystemOp::ListDirectory => ty,
            FileSystemOp::WriteBytes => self.type_of(*args.get(1)?),
            _ => return None,
        };
        self.codegen.element_of(array).ok()
    }

    /// Whether the helper answers with a C truth byte or with a value that is
    /// already what Kira wants.
    fn file_system_result_kind(&self, op: FileSystemOp) -> ResultKind {
        match op {
            FileSystemOp::ReadRange
            | FileSystemOp::ReadText
            | FileSystemOp::ListDirectory
            | FileSystemOp::FileSize => ResultKind::Value,
            FileSystemOp::WriteBytes
            | FileSystemOp::WriteText
            | FileSystemOp::IsDirectory
            | FileSystemOp::MakeDirectory
            | FileSystemOp::RenamePath
            | FileSystemOp::RemovePath
            | FileSystemOp::FileExists
            | FileSystemOp::PathExists => ResultKind::Flag,
        }
    }

    /// Narrows a runtime `i8` of 0 or 1 to the `i1` a Kira `Bool` is.
    ///
    /// Shared with the environment lowering, which gets its truth byte from
    /// `kira_rt_env_is_set` the same way this gets one from `kira_rt_fs_*`.
    pub(in crate::codegen) fn byte_to_bool(&mut self, value: LLVMValueRef) -> LLVMValueRef {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: the helper returns an `i8` of 0 or 1, and the builder is
        // positioned on a live block.
        unsafe {
            let zero = LLVMConstInt(types.i8, 0, 0);
            LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntNE,
                value,
                zero,
                c"rt.flag".as_ptr(),
            )
        }
    }
}

/// What shape a helper's return value arrives in.
enum ResultKind {
    /// A C truth byte, narrowed to `i1`.
    Flag,
    /// Already the value Kira wants: a handle, or an `i64`.
    Value,
}
