//! Lowering for the floating-point primitives.
//!
//! Each becomes the LLVM intrinsic or libm call the target already has, so
//! `sqrt(x)` is a `sqrtsd` on x86 rather than eight Newton iterations. The
//! declaration is by name: `llvm.sqrt.f64` and friends are recognised by the
//! optimiser and lowered to an instruction where one exists, and to a libm call
//! where one does not.
//!
//! `tan` has no LLVM intrinsic — it is the one of these the ISAs do not carry —
//! so it calls libm's `tan` directly. That is what the intrinsic would have
//! become anyway.

use std::ffi::CString;

use kira_runtime_abi::MathOp;
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;
use crate::codegen::types::Callable;

impl FunctionLowering<'_, '_> {
    /// Lowers `sqrt(x)` and the rest to a call on the target's own maths.
    pub(super) fn lower_math_operation(
        &mut self,
        op: MathOp,
        value: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(value)?;
        let callee = self.codegen.math_callable(op);
        // SAFETY: the callee takes one `double` and returns one, and `value` is
        // a `double` on the live current block.
        Ok(unsafe {
            self.codegen
                .call_runtime(callee, &mut [value], c"math.call")
        })
    }
}

impl FunctionLowering<'_, '_> {
    /// Lowers an array argument to the address of its elements in C's widths.
    ///
    /// The runtime does the writing: it already owns the array's layout, and a
    /// loop emitted here would be the same walk in a place that knows less.
    pub(super) fn lower_array_elements(
        &mut self,
        value: kira_ir::IrExprId,
        element: kira_runtime_abi::ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        // The Kira stride, which is not the seam width: a `[F32]` holds `double`
        // elements and writes four bytes each. LLVM's own answer for the target
        // rather than one computed here, for the reason `abi_size` gives.
        let kira_ty = self.type_of(value);
        let element_ty = self
            .codegen
            .program
            .types
            .element_of(kira_ty)
            .unwrap_or(Type::Float(kira_semantics_model::FloatSpelling::Plain));
        let stride = self.codegen.abi_size(element_ty)?;
        let array = self.lower_expr(value)?;
        let types = self.codegen.types;
        let callee = self.codegen.runtime.array_elements;
        // SAFETY: the runtime takes an array handle, the element's seam tag and
        // the Kira stride, and answers a pointer word; all are live here.
        Ok(unsafe {
            let tag = LLVMConstInt(types.i32, u64::from(element.tag()), 0);
            self.codegen
                .call_runtime(callee, &mut [array, tag, stride], c"array.elements")
        })
    }

    /// Lowers `scalarText(codePoint)` to the runtime that encodes it.
    pub(super) fn lower_scalar_text(
        &mut self,
        value: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(value)?;
        let callee = self.codegen.runtime.scalar_text;
        // SAFETY: the runtime takes one `i64` code point and answers a string
        // handle, and `value` is an `i64` on the live current block.
        Ok(unsafe {
            self.codegen
                .call_runtime(callee, &mut [value], c"scalar.text")
        })
    }
}

impl crate::codegen::Codegen<'_> {
    /// The declaration one floating-point operation calls, adding it once.
    pub(crate) fn math_callable(&mut self, op: MathOp) -> Callable {
        let symbol = match op {
            MathOp::Sqrt => "llvm.sqrt.f64",
            MathOp::Sin => "llvm.sin.f64",
            MathOp::Cos => "llvm.cos.f64",
            MathOp::Floor => "llvm.floor.f64",
            MathOp::Ceil => "llvm.ceil.f64",
            MathOp::Abs => "llvm.fabs.f64",
            // No intrinsic exists; libm is what one would lower to.
            MathOp::Tan => "tan",
        };
        let name = CString::new(symbol).expect("a maths symbol holds no NUL");
        // SAFETY: the module is live, and the type is the one every declaration
        // here shares — `double(double)`.
        unsafe {
            let ty = LLVMFunctionType(self.types.f64, [self.types.f64].as_mut_ptr(), 1, 0);
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let value = if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                existing
            };
            Callable { ty, value }
        }
    }
}
