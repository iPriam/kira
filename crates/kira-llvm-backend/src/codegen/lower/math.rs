//! Lowering for the floating-point primitives.
//!
//! Each becomes the LLVM intrinsic or libm call the target already has, so
//! `sqrt(x)` is a `sqrtsd` on x86 rather than eight Newton iterations. The
//! declaration is by name: `llvm.sqrt.f64` and friends are recognised by the
//! optimiser and lowered to an instruction where one exists, and to a libm call
//! where one does not.
//!
//! The transcendentals LLVM has no intrinsic for — `tan`, the inverse and
//! hyperbolic trigonometry, `atan2`, `hypot` and `fmod` — call libm by name
//! directly. That is what an intrinsic would have become anyway.
//!
//! An operation takes one operand or two, so the declaration's signature is
//! built from its own `argument_count` rather than fixed at `double(double)`.

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
        operands: &[kira_ir::IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        // In source order, which is the order the declaration takes them in:
        // `pow(x, y)` is x to the y and `atan2(y, x)` takes its quadrant from
        // both, so the two are not interchangeable.
        let mut arguments = Vec::with_capacity(operands.len());
        for &operand in operands {
            arguments.push(self.lower_expr(operand)?);
        }
        let callee = self.codegen.math_callable(op)?;
        // SAFETY: the callee takes `argument_count` `double`s and returns one;
        // the typechecker coerced every operand to `Float`, so each is a
        // `double` on the live current block.
        Ok(unsafe {
            self.codegen
                .call_runtime(callee, &mut arguments, c"math.call")
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
    ///
    /// The name is only taken when it is free or already carries exactly this
    /// `double(double)` shape: a foreign import of a same-named C symbol
    /// declares first with its own signature, and calling through that would
    /// hand libm's callee the wrong type — a verifier failure far from the
    /// line that caused it.
    pub(crate) fn math_callable(&mut self, op: MathOp) -> Result<Callable, LlvmError> {
        let name: &std::ffi::CStr = match op {
            MathOp::Sqrt => c"llvm.sqrt.f64",
            MathOp::Sin => c"llvm.sin.f64",
            MathOp::Cos => c"llvm.cos.f64",
            MathOp::Floor => c"llvm.floor.f64",
            MathOp::Ceil => c"llvm.ceil.f64",
            MathOp::Abs => c"llvm.fabs.f64",
            MathOp::Exp => c"llvm.exp.f64",
            MathOp::Log => c"llvm.log.f64",
            MathOp::Log2 => c"llvm.log2.f64",
            MathOp::Log10 => c"llvm.log10.f64",
            MathOp::Exp2 => c"llvm.exp2.f64",
            MathOp::Round => c"llvm.round.f64",
            MathOp::Trunc => c"llvm.trunc.f64",
            MathOp::Pow => c"llvm.pow.f64",
            MathOp::CopySign => c"llvm.copysign.f64",
            // `minnum`/`maxnum` are IEEE-754's: given one NaN they answer with
            // the other operand, which is what `f64::min` does and therefore
            // what the VM already answers.
            MathOp::Min => c"llvm.minnum.f64",
            MathOp::Max => c"llvm.maxnum.f64",
            // No intrinsic exists; libm is what one would lower to.
            MathOp::Tan => c"tan",
            MathOp::Asin => c"asin",
            MathOp::Acos => c"acos",
            MathOp::Atan => c"atan",
            MathOp::Sinh => c"sinh",
            MathOp::Cosh => c"cosh",
            MathOp::Tanh => c"tanh",
            MathOp::Atan2 => c"atan2",
            MathOp::Hypot => c"hypot",
            MathOp::Fmod => c"fmod",
        };
        // SAFETY: the module is live, and the type is `double(double...)` with
        // as many parameters as the operation takes operands.
        unsafe {
            let mut parameters = [self.types.f64; MathOp::MAX_ARGUMENTS];
            let arity = op.argument_count();
            let ty = LLVMFunctionType(
                self.types.f64,
                parameters[..arity].as_mut_ptr(),
                arity as u32,
                0,
            );
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let value = if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                let found = LLVMGlobalGetValueType(existing);
                if found != ty {
                    return Err(LlvmError::SymbolCollision {
                        symbol: name.to_string_lossy().into_owned(),
                    });
                }
                existing
            };
            Ok(Callable { ty, value })
        }
    }
}
