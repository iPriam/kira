//! Integer width and conversion semantics.
//!
//! An integer is one `i64` whatever its spelling, so the width rules are
//! control flow around the arithmetic: an overflow intrinsic whose flag
//! branches to a trap, a range compare after arithmetic at a narrower width,
//! a compare on a shift count, and a checked conversion between spellings.
//! Each trap calls the runtime function the VM's error of the same name
//! mirrors, so a program traps identically on both engines.

use std::ffi::CStr;

use kira_ir::{ConvertKind, IrBinOp, IrUnOp};
use kira_runtime_abi::IntWidth;
use kira_semantics_model::{IntSpelling, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMRealPredicate};

use super::FunctionLowering;
use crate::LlvmError;
use crate::codegen::types::Callable;

impl FunctionLowering<'_, '_> {
    /// Branches to a block that calls `trap` with `args` and never returns
    /// when `condition` holds; leaves the builder on the continuing block.
    pub(super) fn trap_if(
        &mut self,
        condition: LLVMValueRef,
        trap: Callable,
        args: &mut [LLVMValueRef],
        label: &CStr,
    ) -> Result<(), LlvmError> {
        let function = self.current_function();
        let trap_block = self.append_block(function, label);
        let continue_block = self.append_block(function, c"continue");
        // SAFETY: both blocks belong to the function being built, the builder
        // is on a live block, and the trap block ends in `unreachable` after a
        // call that never returns.
        unsafe {
            LLVMBuildCondBr(self.codegen.builder, condition, trap_block, continue_block);
            LLVMPositionBuilderAtEnd(self.codegen.builder, trap_block);
            self.codegen.call_runtime(trap, args, c"");
            LLVMBuildUnreachable(self.codegen.builder);
        }
        self.position_at(continue_block);
        Ok(())
    }

    /// An `i32` constant carrying a width's code, for a trap's argument.
    fn width_code(&self, width: IntWidth) -> LLVMValueRef {
        // SAFETY: a constant of a live type.
        unsafe { LLVMConstInt(self.codegen.types.i32, u64::from(width.code()), 0) }
    }

    /// `llvm.<name>.with.overflow.i64` applied to `left` and `right`,
    /// trapping when the flag is set; the result otherwise.
    fn checked_arithmetic(
        &mut self,
        intrinsic: &CStr,
        left: LLVMValueRef,
        right: LLVMValueRef,
        width: IntWidth,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.codegen.types;
        // SAFETY: the intrinsic exists for `i64` on every target, both
        // operands are `i64`, and the builder is on a live block. The
        // aggregate it returns is `{ i64, i1 }`, read by index.
        let (value, overflowed) = unsafe {
            let id = LLVMLookupIntrinsicID(intrinsic.as_ptr(), intrinsic.to_bytes().len());
            let mut params = [types.i64];
            let callee = LLVMGetIntrinsicDeclaration(
                self.codegen.module,
                id,
                params.as_mut_ptr(),
                params.len(),
            );
            let callee_ty = LLVMIntrinsicGetType(
                self.codegen.context,
                id,
                params.as_mut_ptr(),
                params.len(),
            );
            let mut args = [left, right];
            let pair = LLVMBuildCall2(
                self.codegen.builder,
                callee_ty,
                callee,
                args.as_mut_ptr(),
                args.len() as u32,
                c"arith".as_ptr(),
            );
            (
                LLVMBuildExtractValue(self.codegen.builder, pair, 0, c"arith.value".as_ptr()),
                LLVMBuildExtractValue(self.codegen.builder, pair, 1, c"arith.overflow".as_ptr()),
            )
        };
        let mut args = [self.width_code(width)];
        self.trap_if(overflowed, self.codegen.runtime.trap_overflow, &mut args, c"overflow")?;
        Ok(value)
    }

    /// Traps unless `value`, read as a signed `i64` result, lies in the
    /// range of `width`; returns it unchanged. Nothing to check at 64 bits.
    pub(super) fn check_width(
        &mut self,
        value: LLVMValueRef,
        width: IntWidth,
    ) -> Result<LLVMValueRef, LlvmError> {
        if width.bits() == 64 {
            return Ok(value);
        }
        let (low, high) = width.range();
        let types = self.codegen.types;
        // SAFETY: constants of a live type and compares on `i64` operands on
        // a live block.
        let outside = unsafe {
            let low = LLVMConstInt(types.i64, low as i64 as u64, 1);
            let high = LLVMConstInt(types.i64, high as i64 as u64, 1);
            let below = LLVMBuildICmp(
                self.codegen.builder,
                LLVMIntPredicate::LLVMIntSLT,
                value,
                low,
                c"width.below".as_ptr(),
            );
            let above = LLVMBuildICmp(
                self.codegen.builder,
                LLVMIntPredicate::LLVMIntSGT,
                value,
                high,
                c"width.above".as_ptr(),
            );
            LLVMBuildOr(self.codegen.builder, below, above, c"width.outside".as_ptr())
        };
        let mut args = [self.width_code(width)];
        self.trap_if(outside, self.codegen.runtime.trap_overflow, &mut args, c"width.trap")?;
        Ok(value)
    }

    /// `value` reduced to `width` the way a shift discards bits: truncated,
    /// then sign- or zero-extended back to `i64`.
    fn wrap_width(&self, value: LLVMValueRef, width: IntWidth) -> LLVMValueRef {
        let types = self.codegen.types;
        let narrow = match width.bits() {
            8 => types.i8,
            16 => types.i16,
            32 => types.i32,
            _ => return value,
        };
        // SAFETY: a truncation and an extension of an `i64` on a live block.
        unsafe {
            let low = LLVMBuildTrunc(self.codegen.builder, value, narrow, c"wrap.low".as_ptr());
            if width.is_signed() {
                LLVMBuildSExt(self.codegen.builder, low, types.i64, c"wrap.sext".as_ptr())
            } else {
                LLVMBuildZExt(self.codegen.builder, low, types.i64, c"wrap.zext".as_ptr())
            }
        }
    }

    /// Traps unless the shift `count` lies in `0..bits`.
    fn check_shift(&mut self, count: LLVMValueRef, width: IntWidth) -> Result<(), LlvmError> {
        let types = self.codegen.types;
        // SAFETY: an unsigned compare of an `i64` against a constant: a
        // negative count is a huge unsigned one, so one compare covers both
        // ends of the range.
        let outside = unsafe {
            let bits = LLVMConstInt(types.i64, u64::from(width.bits()), 0);
            LLVMBuildICmp(
                self.codegen.builder,
                LLVMIntPredicate::LLVMIntUGE,
                count,
                bits,
                c"shift.outside".as_ptr(),
            )
        };
        // SAFETY: a constant of a live type.
        let bits = unsafe { LLVMConstInt(types.i32, u64::from(width.bits()), 0) };
        let mut args = [count, bits];
        self.trap_if(outside, self.codegen.runtime.trap_shift, &mut args, c"shift.trap")
    }

    /// Integer arithmetic and shifts at `spelling`'s width, or `None` when
    /// `op` is not one of them and the caller's plain lowering applies.
    pub(super) fn lower_int_arithmetic(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
        spelling: IntSpelling,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let width = spelling.width();
        let builder = self.codegen.builder;
        let value = match op {
            IrBinOp::AddInt | IrBinOp::SubInt | IrBinOp::MulInt => {
                let intrinsic: &CStr = match (op, width.is_signed()) {
                    (IrBinOp::AddInt, true) => c"llvm.sadd.with.overflow",
                    (IrBinOp::SubInt, true) => c"llvm.ssub.with.overflow",
                    (IrBinOp::MulInt, true) => c"llvm.smul.with.overflow",
                    (IrBinOp::AddInt, false) => c"llvm.uadd.with.overflow",
                    (IrBinOp::SubInt, false) => c"llvm.usub.with.overflow",
                    (_, false) => c"llvm.umul.with.overflow",
                    _ => unreachable!("the match arm admits only add, sub, and mul"),
                };
                // The narrow unsigned spellings are stored zero-extended, so
                // their 64-bit signed arithmetic cannot overflow before the
                // range check catches it; only `U64` needs the unsigned
                // intrinsic, and only 64-bit values need any intrinsic.
                let checked = if width.bits() == 64 || width.is_signed() {
                    self.checked_arithmetic(intrinsic, left, right, width)?
                } else {
                    // SAFETY: plain `i64` arithmetic on a live block.
                    unsafe {
                        match op {
                            IrBinOp::AddInt => LLVMBuildAdd(builder, left, right, c"add".as_ptr()),
                            IrBinOp::SubInt => LLVMBuildSub(builder, left, right, c"sub".as_ptr()),
                            _ => LLVMBuildMul(builder, left, right, c"mul".as_ptr()),
                        }
                    }
                };
                self.check_width(checked, width)?
            }
            IrBinOp::DivInt => {
                let divided = self.lower_int_division(op, left, right)?;
                self.check_width(divided, width)?
            }
            IrBinOp::WrappingAddInt | IrBinOp::WrappingSubInt | IrBinOp::WrappingMulInt => {
                // SAFETY: plain `i64` arithmetic on a live block; no
                // `nsw`/`nuw`, so it wraps at 64 bits before the width wrap.
                let wrapped = unsafe {
                    match op {
                        IrBinOp::WrappingAddInt => {
                            LLVMBuildAdd(builder, left, right, c"wadd".as_ptr())
                        }
                        IrBinOp::WrappingSubInt => {
                            LLVMBuildSub(builder, left, right, c"wsub".as_ptr())
                        }
                        _ => LLVMBuildMul(builder, left, right, c"wmul".as_ptr()),
                    }
                };
                self.wrap_width(wrapped, width)
            }
            IrBinOp::Shl | IrBinOp::ShrInt | IrBinOp::ShrUInt => {
                self.check_shift(right, width)?;
                // SAFETY: the count was just proven below the width, so the
                // shift is defined; operands are `i64` on a live block.
                let shifted = unsafe {
                    match op {
                        IrBinOp::Shl => LLVMBuildShl(builder, left, right, c"shl".as_ptr()),
                        IrBinOp::ShrInt => LLVMBuildAShr(builder, left, right, c"ashr".as_ptr()),
                        _ => LLVMBuildLShr(builder, left, right, c"lshr".as_ptr()),
                    }
                };
                if op == IrBinOp::Shl {
                    self.wrap_width(shifted, width)
                } else {
                    shifted
                }
            }
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    /// A unary operator with its result type: negation traps on `MIN` and
    /// is range-checked at a narrower width.
    pub(super) fn lower_unary_typed(
        &mut self,
        op: IrUnOp,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let (IrUnOp::NegInt, Type::Int(spelling)) = (op, ty) {
            let width = spelling.width();
            let types = self.codegen.types;
            // SAFETY: `0 - value` through the overflow intrinsic on a live
            // block; a constant of a live type.
            let zero = unsafe { LLVMConstInt(types.i64, 0, 0) };
            let negated = self.checked_arithmetic(c"llvm.ssub.with.overflow", zero, value, width)?;
            return self.check_width(negated, width);
        }
        Ok(self.lower_unary(op, value))
    }

    /// A scalar conversion with the source and destination types, so an
    /// integer-to-integer conversion can be checked and an unsigned source
    /// converts to float as the value it is.
    pub(super) fn lower_convert_typed(
        &mut self,
        kind: ConvertKind,
        value: LLVMValueRef,
        from: Type,
        to: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let from_width = match from {
            Type::Int(spelling) => spelling.width(),
            _ => IntWidth::Plain,
        };
        match kind {
            ConvertKind::IntToInt => {
                let Type::Int(to_spelling) = to else {
                    return Ok(value);
                };
                let to_width = to_spelling.width();
                if from_width.widens_into(to_width) {
                    return Ok(value);
                }
                self.check_conversion(value, from_width, to_width)?;
                Ok(value)
            }
            ConvertKind::IntToFloat if from_width == IntWidth::U64 => {
                let types = self.codegen.types;
                // SAFETY: `value` is an `i64` on a live block.
                Ok(unsafe {
                    LLVMBuildUIToFP(self.codegen.builder, value, types.f64, c"conv.uitofp".as_ptr())
                })
            }
            ConvertKind::FloatToInt => {
                let converted = self.lower_float_to_int(value)?;
                if let Type::Int(to_spelling) = to
                    && to_spelling.width() != IntWidth::Plain
                {
                    self.check_conversion(converted, IntWidth::Plain, to_spelling.width())?;
                }
                Ok(converted)
            }
            _ => Ok(self.lower_convert(kind, value)),
        }
    }

    /// Traps unless `value`, read under `from`, lies in the range of `to`.
    fn check_conversion(
        &mut self,
        value: LLVMValueRef,
        from: IntWidth,
        to: IntWidth,
    ) -> Result<(), LlvmError> {
        let types = self.codegen.types;
        let (low, high) = to.range();
        // Under `from`'s reading of the word, the destination range is a pair
        // of bounds compared with `from`'s signedness. A `U64` word reads
        // unsigned, so its bounds are unsigned and its compare is too; every
        // other source reads signed. A destination bound outside the source's
        // own range needs no compare at all.
        let (from_low, from_high) = from.range();
        let signed = from.is_signed();
        let predicate_lt = if signed {
            LLVMIntPredicate::LLVMIntSLT
        } else {
            LLVMIntPredicate::LLVMIntULT
        };
        let predicate_gt = if signed {
            LLVMIntPredicate::LLVMIntSGT
        } else {
            LLVMIntPredicate::LLVMIntUGT
        };
        // SAFETY: constants of a live type and compares of `i64` operands on
        // a live block.
        let outside = unsafe {
            let falsehood = LLVMConstInt(types.i1, 0, 0);
            let below = if low > from_low {
                let bound = LLVMConstInt(types.i64, from.word_of(low) as u64, 1);
                LLVMBuildICmp(self.codegen.builder, predicate_lt, value, bound, c"conv.below".as_ptr())
            } else {
                falsehood
            };
            let above = if high < from_high {
                let bound = LLVMConstInt(types.i64, from.word_of(high) as u64, 1);
                LLVMBuildICmp(self.codegen.builder, predicate_gt, value, bound, c"conv.above".as_ptr())
            } else {
                falsehood
            };
            LLVMBuildOr(self.codegen.builder, below, above, c"conv.outside".as_ptr())
        };
        let mut args = [value, self.width_code(from), self.width_code(to)];
        self.trap_if(outside, self.codegen.runtime.trap_narrow, &mut args, c"conv.trap")
    }

    /// Truncates a float toward zero, trapping on NaN, an infinity, or a
    /// value outside the 64-bit signed range — where a bare `fptosi` would
    /// be poison.
    fn lower_float_to_int(&mut self, value: LLVMValueRef) -> Result<LLVMValueRef, LlvmError> {
        let types = self.codegen.types;
        // SAFETY: float compares against constants on a live block. An
        // ordered compare is false for NaN, so `in range` is false for NaN
        // and the trap fires; `2^63` is exactly the first float past the top
        // and is excluded, while `-2^63` is exactly `i64::MIN` and included
        // by the non-strict compare.
        let outside = unsafe {
            let low = LLVMConstReal(types.f64, -9_223_372_036_854_775_808.0);
            let high = LLVMConstReal(types.f64, 9_223_372_036_854_775_808.0);
            let above_low = LLVMBuildFCmp(
                self.codegen.builder,
                LLVMRealPredicate::LLVMRealOGE,
                value,
                low,
                c"conv.f.low".as_ptr(),
            );
            let below_high = LLVMBuildFCmp(
                self.codegen.builder,
                LLVMRealPredicate::LLVMRealOLT,
                value,
                high,
                c"conv.f.high".as_ptr(),
            );
            let inside = LLVMBuildAnd(self.codegen.builder, above_low, below_high, c"conv.f.in".as_ptr());
            LLVMBuildNot(self.codegen.builder, inside, c"conv.f.out".as_ptr())
        };
        let mut args = [value];
        self.trap_if(outside, self.codegen.runtime.trap_float_to_int, &mut args, c"conv.f.trap")?;
        // SAFETY: the value was just proven in range, so `fptosi` is defined.
        Ok(unsafe {
            LLVMBuildFPToSI(self.codegen.builder, value, types.i64, c"conv.fptosi".as_ptr())
        })
    }
}
