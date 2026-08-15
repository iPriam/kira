//! Operator lowering: unary, binary, division, comparison, and short-circuit.
//!
//! Split from [`super::expr`] on the file-size ladder, and cohesive on its own:
//! this is where native code and the interpreter agree or fail to. Wrapping
//! integer arithmetic, the two trapping divisions, and the short-circuit
//! operators that are control flow rather than instructions all live here, so
//! the rules the VM fixes have one place to be mirrored.

use kira_ir::{ConvertKind, IrBinOp, IrExprId, IrUnOp};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMRealPredicate};

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a unary operator.
    pub(super) fn lower_unary(&mut self, op: IrUnOp, value: LLVMValueRef) -> LLVMValueRef {
        let builder = self.codegen.builder;
        // SAFETY: `value` has the operand type the typed operator fixes, and
        // the builder is on a live block. `LLVMBuildNeg` carries no `nsw`, so
        // it wraps like the VM's `wrapping_neg`.
        unsafe {
            match op {
                IrUnOp::NegInt => LLVMBuildNeg(builder, value, c"neg".as_ptr()),
                IrUnOp::NegFloat => LLVMBuildFNeg(builder, value, c"fneg".as_ptr()),
                // `!` on an `i1` and `~` on an `i64` are the same instruction:
                // LLVM's `not` complements every bit of whatever width it is
                // given, which for a one-bit boolean is logical negation.
                IrUnOp::Not | IrUnOp::BitNot => LLVMBuildNot(builder, value, c"not".as_ptr()),
            }
        }
    }

    /// Lowers a scalar numeric conversion with the VM's exact semantics.
    ///
    /// Two of the four kinds are identity copies — an integer width and a float
    /// width are annotations over one runtime representation — so the value is
    /// returned unchanged. `IntToFloat` is a signed `sitofp`. `FloatToInt` must
    /// **saturate**: a bare `fptosi` is poison on an out-of-range or NaN input,
    /// while the VM's `f64 as i64` is total. The select chain mirrors the VM
    /// exactly — clamp at or above `(f64)i64::MAX`, clamp at or below
    /// `(f64)i64::MIN`, and map NaN to zero — each with a pure `fcmp` and
    /// constant, so no branch and no poison reaches the result.
    pub(super) fn lower_convert(&mut self, kind: ConvertKind, value: LLVMValueRef) -> LLVMValueRef {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        match kind {
            ConvertKind::IntToInt
            | ConvertKind::FloatToFloat
            | ConvertKind::IntToRawPtr
            | ConvertKind::RawPtrToInt => value,
            // SAFETY: `value` is the `i64` the typed conversion fixes and the
            // builder is on a live block; the call builds a pure value.
            ConvertKind::IntToFloat => unsafe {
                LLVMBuildSIToFP(builder, value, types.f64, c"conv.sitofp".as_ptr())
            },
            // A reinterpretation of the same 64 bits, which is exactly what the
            // VM's `to_bits`/`from_bits` do.
            //
            // SAFETY: `value` has the type the conversion fixes and the builder
            // is on a live block; a bitcast builds a pure value.
            ConvertKind::FloatToBits => unsafe {
                LLVMBuildBitCast(builder, value, types.i64, c"conv.f2b".as_ptr())
            },
            // SAFETY: as above, in the other direction.
            ConvertKind::BitsToFloat => unsafe {
                LLVMBuildBitCast(builder, value, types.f64, c"conv.b2f".as_ptr())
            },
            // SAFETY: as above. The truncation takes the low 32 bits, which are
            // the pattern; the widening is a numeric conversion applied *after*
            // the reinterpretation, because the same bits denote a different
            // number at the two widths.
            ConvertKind::Bits32ToFloat => unsafe {
                let narrow = LLVMBuildTrunc(builder, value, types.i32, c"conv.b32".as_ptr());
                let single = LLVMBuildBitCast(builder, narrow, types.f32, c"conv.b2f32".as_ptr());
                LLVMBuildFPExt(builder, single, types.f64, c"conv.f32f64".as_ptr())
            },
            // SAFETY: as above, in the other direction. `fptrunc` rounds to
            // nearest even — the same rounding the VM's `as f32` does — and the
            // pattern is zero-extended, so the `U32` never comes back negative.
            ConvertKind::FloatToBits32 => unsafe {
                let single = LLVMBuildFPTrunc(builder, value, types.f32, c"conv.f64f32".as_ptr());
                let bits = LLVMBuildBitCast(builder, single, types.i32, c"conv.f2b32".as_ptr());
                LLVMBuildZExt(builder, bits, types.i64, c"conv.b32ext".as_ptr())
            },
            // SAFETY: `value` is the `f64` the typed conversion fixes and the
            // builder is on a live block; every call below builds a pure value,
            // and the selected operand is never the poison `fptosi` for an input
            // outside the in-range interval where `fptosi` is defined.
            ConvertKind::FloatToInt => unsafe {
                let raw = LLVMBuildFPToSI(builder, value, types.i64, c"conv.fptosi".as_ptr());
                let max_f = LLVMConstReal(types.f64, i64::MAX as f64);
                let min_f = LLVMConstReal(types.f64, i64::MIN as f64);
                let max_i = LLVMConstInt(types.i64, i64::MAX as u64, 0);
                let min_i = LLVMConstInt(types.i64, i64::MIN as u64, 0);
                let zero_i = LLVMConstInt(types.i64, 0, 0);
                let ge_max = LLVMBuildFCmp(
                    builder,
                    LLVMRealPredicate::LLVMRealOGE,
                    value,
                    max_f,
                    c"conv.ge.max".as_ptr(),
                );
                let clamped_hi = LLVMBuildSelect(builder, ge_max, max_i, raw, c"conv.hi".as_ptr());
                let le_min = LLVMBuildFCmp(
                    builder,
                    LLVMRealPredicate::LLVMRealOLE,
                    value,
                    min_f,
                    c"conv.le.min".as_ptr(),
                );
                let clamped_lo =
                    LLVMBuildSelect(builder, le_min, min_i, clamped_hi, c"conv.lo".as_ptr());
                // An unordered compare of a value with itself is true iff it is
                // NaN.
                let is_nan = LLVMBuildFCmp(
                    builder,
                    LLVMRealPredicate::LLVMRealUNO,
                    value,
                    value,
                    c"conv.nan".as_ptr(),
                );
                LLVMBuildSelect(builder, is_nan, zero_i, clamped_lo, c"conv.int".as_ptr())
            },
        }
    }

    /// Lowers a binary operator.
    pub(super) fn lower_binary(
        &mut self,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        // Short-circuit operators are control flow, not instructions: the VM
        // never evaluates the right operand unless the left demands it.
        match op {
            IrBinOp::And => return self.lower_short_circuit(lhs, rhs, false),
            IrBinOp::Or => return self.lower_short_circuit(lhs, rhs, true),
            _ => {}
        }

        let left = self.lower_expr(lhs)?;
        let right = self.lower_expr(rhs)?;
        let builder = self.codegen.builder;

        // SAFETY: both operands carry the types the typed operator fixes, and the
        // builder is on a live block. None of the integer builders set
        // `nsw`/`nuw`, so they wrap as the VM does.
        let value = unsafe {
            match op {
                IrBinOp::AddInt => LLVMBuildAdd(builder, left, right, c"add".as_ptr()),
                IrBinOp::SubInt => LLVMBuildSub(builder, left, right, c"sub".as_ptr()),
                IrBinOp::MulInt => LLVMBuildMul(builder, left, right, c"mul".as_ptr()),
                IrBinOp::DivInt | IrBinOp::RemInt => {
                    return self.lower_int_division(op, left, right);
                }
                IrBinOp::DivUInt | IrBinOp::RemUInt => {
                    return self.lower_uint_division(op, left, right);
                }
                IrBinOp::AddFloat => LLVMBuildFAdd(builder, left, right, c"fadd".as_ptr()),
                IrBinOp::SubFloat => LLVMBuildFSub(builder, left, right, c"fsub".as_ptr()),
                IrBinOp::MulFloat => LLVMBuildFMul(builder, left, right, c"fmul".as_ptr()),
                IrBinOp::DivFloat => LLVMBuildFDiv(builder, left, right, c"fdiv".as_ptr()),
                IrBinOp::RemFloat => LLVMBuildFRem(builder, left, right, c"frem".as_ptr()),
                IrBinOp::ConcatStr => {
                    return Ok(self.call(
                        self.codegen.runtime.str_concat,
                        &mut [left, right],
                        c"str.concat",
                    ));
                }
                IrBinOp::EqStr | IrBinOp::NeStr => {
                    return Ok(self.lower_string_compare(op, left, right));
                }
                // Both operands are erasure boxes, and the runtime reads the
                // type each carries before it reads either payload. The two are
                // dropped afterwards, as every comparison drops what it
                // consumed — `kira_rt_any_eq` takes nothing.
                IrBinOp::EqAny | IrBinOp::NeAny => {
                    let equal =
                        self.call(self.codegen.runtime.any_eq, &mut [left, right], c"any.eq");
                    self.drop_value(left, Type::Any)?;
                    self.drop_value(right, Type::Any)?;
                    let builder = self.codegen.builder;
                    let types = self.codegen.types;
                    // The helper returns an `i8` of 0 or 1; comparing it
                    // against the appropriate constant yields the `i1` Kira
                    // booleans are, exactly as the string compare does.
                    let expected = LLVMConstInt(types.i8, u64::from(op == IrBinOp::EqAny), 0);
                    return Ok(LLVMBuildICmp(
                        builder,
                        LLVMIntPredicate::LLVMIntEQ,
                        equal,
                        expected,
                        c"any.cmp".as_ptr(),
                    ));
                }
                IrBinOp::BitAnd => LLVMBuildAnd(builder, left, right, c"and".as_ptr()),
                IrBinOp::BitOr => LLVMBuildOr(builder, left, right, c"or".as_ptr()),
                IrBinOp::BitXor => LLVMBuildXor(builder, left, right, c"xor".as_ptr()),
                // The one place LLVM would disagree with the VM: a shift by 64
                // or more is *poison* for `shl`/`lshr`/`ashr`, where the VM and
                // wasm both take the amount modulo 64. Masking the amount here
                // is what makes the three backends agree, and it costs an `and`
                // that the optimizer drops whenever the count is a constant in
                // range.
                IrBinOp::Shl | IrBinOp::ShrInt | IrBinOp::ShrUInt => {
                    let mask = LLVMConstInt(self.codegen.types.i64, 63, 0);
                    let amount = LLVMBuildAnd(builder, right, mask, c"shamt".as_ptr());
                    match op {
                        IrBinOp::Shl => LLVMBuildShl(builder, left, amount, c"shl".as_ptr()),
                        IrBinOp::ShrUInt => LLVMBuildLShr(builder, left, amount, c"lshr".as_ptr()),
                        _ => LLVMBuildAShr(builder, left, amount, c"ashr".as_ptr()),
                    }
                }
                other => {
                    let predicate = integer_predicate(other);
                    match predicate {
                        Some(predicate) => {
                            LLVMBuildICmp(builder, predicate, left, right, c"icmp".as_ptr())
                        }
                        None => {
                            let predicate = real_predicate(other).ok_or(LlvmError::internal(
                                "an operator with no native lowering",
                            ))?;
                            LLVMBuildFCmp(builder, predicate, left, right, c"fcmp".as_ptr())
                        }
                    }
                }
            }
        };
        Ok(value)
    }

    /// Lowers `/` or `%` with the VM's exact semantics.
    ///
    /// Two cases LLVM would get wrong on its own: a zero divisor is a trap in
    /// Kira (not UB), and `MIN / -1` overflows — poison for `sdiv`, but a
    /// defined wrapping result for the VM's `wrapping_div`. Both are branched
    /// on explicitly, so the fast path stays a plain `sdiv`/`srem`.
    fn lower_int_division(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        let function = self.current_function();
        let trap_block = self.append_block(function, c"div.trap");
        let overflow_block = self.append_block(function, c"div.overflow");
        let normal_block = self.append_block(function, c"div.normal");
        let done_block = self.append_block(function, c"div.done");

        // SAFETY: every block belongs to the function being built, both
        // operands are `i64`, and each block is terminated exactly once below.
        let (overflow_value, normal_value) = unsafe {
            let zero = LLVMConstInt(types.i64, 0, 0);
            let by_zero = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                right,
                zero,
                c"div.by.zero".as_ptr(),
            );
            LLVMBuildCondBr(builder, by_zero, trap_block, overflow_block);

            // Divisor is zero: the runtime reports the trap and exits, so
            // nothing follows.
            LLVMPositionBuilderAtEnd(builder, trap_block);
            self.codegen
                .call_runtime(self.codegen.runtime.trap_div_zero, &mut [], c"");
            LLVMBuildUnreachable(builder);

            // Divisor is -1: `MIN / -1` would be poison, so take the wrapping
            // answer directly. `x / -1` is `-x` and `x % -1` is 0 for every x,
            // so this branch needs no division at all.
            LLVMPositionBuilderAtEnd(builder, overflow_block);
            let minus_one = LLVMConstInt(types.i64, u64::MAX, 1);
            let by_minus_one = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                right,
                minus_one,
                c"div.by.minus.one".as_ptr(),
            );
            let wrap_block = self.append_block(function, c"div.wrap");
            LLVMBuildCondBr(builder, by_minus_one, wrap_block, normal_block);

            LLVMPositionBuilderAtEnd(builder, wrap_block);
            let wrapped = match op {
                IrBinOp::DivInt => LLVMBuildNeg(builder, left, c"div.wrapped".as_ptr()),
                _ => zero,
            };
            LLVMBuildBr(builder, done_block);
            let wrap_exit = LLVMGetInsertBlock(builder);

            LLVMPositionBuilderAtEnd(builder, normal_block);
            let divided = match op {
                IrBinOp::DivInt => LLVMBuildSDiv(builder, left, right, c"div".as_ptr()),
                _ => LLVMBuildSRem(builder, left, right, c"rem".as_ptr()),
            };
            LLVMBuildBr(builder, done_block);
            let normal_exit = LLVMGetInsertBlock(builder);

            ((wrapped, wrap_exit), (divided, normal_exit))
        };

        self.position_at(done_block);
        // SAFETY: the phi joins the two predecessors just built, both carrying
        // an `i64`.
        let result = unsafe {
            let phi = LLVMBuildPhi(builder, types.i64, c"div.result".as_ptr());
            let mut values = [overflow_value.0, normal_value.0];
            let mut blocks = [overflow_value.1, normal_value.1];
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            phi
        };
        Ok(result)
    }

    /// Lowers `/` or `%` for the `U8`..`U64` spellings.
    ///
    /// Shorter than its signed twin by one branch, and necessarily so: no pair
    /// of unsigned operands overflows, so there is no `MIN / -1` case to fold
    /// away and no phi to join. Only the divide-by-zero trap remains, which is
    /// the same trap the VM and wasm raise on the same input.
    fn lower_uint_division(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        let function = self.current_function();
        let trap_block = self.append_block(function, c"udiv.trap");
        let normal_block = self.append_block(function, c"udiv.normal");

        // SAFETY: both blocks belong to the function being built, both operands
        // are `i64`, and each block is terminated exactly once below.
        let result = unsafe {
            let zero = LLVMConstInt(types.i64, 0, 0);
            let by_zero = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                right,
                zero,
                c"udiv.by.zero".as_ptr(),
            );
            LLVMBuildCondBr(builder, by_zero, trap_block, normal_block);

            LLVMPositionBuilderAtEnd(builder, trap_block);
            self.codegen
                .call_runtime(self.codegen.runtime.trap_div_zero, &mut [], c"");
            LLVMBuildUnreachable(builder);

            LLVMPositionBuilderAtEnd(builder, normal_block);
            match op {
                IrBinOp::DivUInt => LLVMBuildUDiv(builder, left, right, c"udiv".as_ptr()),
                _ => LLVMBuildURem(builder, left, right, c"urem".as_ptr()),
            }
        };
        Ok(result)
    }

    /// Lowers `==`/`!=` on strings through the runtime helper.
    fn lower_string_compare(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
    ) -> LLVMValueRef {
        let equal = self.call(self.codegen.runtime.str_eq, &mut [left, right], c"str.eq");
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: the helper returns an `i8` of 0 or 1; comparing it against
        // the appropriate constant yields the `i1` Kira booleans are.
        unsafe {
            let expected = LLVMConstInt(types.i8, u64::from(op == IrBinOp::EqStr), 0);
            LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                equal,
                expected,
                c"str.cmp".as_ptr(),
            )
        }
    }

    /// Lowers `cond ? then : otherwise`.
    ///
    /// A branch and a phi, not `LLVMBuildSelect`: a select evaluates **both**
    /// operands, which a conditional expression must never do — one branch may
    /// divide by zero, index out of bounds, or call a function with an effect.
    /// This is the same shape [`Self::lower_short_circuit`] uses, generalized
    /// to any result type instead of `i1`.
    pub(super) fn lower_select(
        &mut self,
        cond: IrExprId,
        then: IrExprId,
        otherwise: IrExprId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let condition = self.lower_expr(cond)?;
        let function = self.current_function();
        let then_block = self.append_block(function, c"sel.then");
        let else_block = self.append_block(function, c"sel.else");
        let done_block = self.append_block(function, c"sel.end");
        let builder = self.codegen.builder;

        // SAFETY: all three blocks belong to the function being built and
        // `condition` is an `i1`; each block is terminated exactly once below.
        unsafe { LLVMBuildCondBr(builder, condition, then_block, else_block) };

        self.position_at(then_block);
        let then_value = self.lower_expr(then)?;
        // SAFETY: the then block is unterminated; join it to the end. The exit
        // block is re-read rather than assumed, because lowering the branch may
        // itself have created blocks (a nested conditional, or a division).
        let then_exit = unsafe {
            LLVMBuildBr(builder, done_block);
            LLVMGetInsertBlock(builder)
        };

        self.position_at(else_block);
        let else_value = self.lower_expr(otherwise)?;
        // SAFETY: as above, for the else branch.
        let else_exit = unsafe {
            LLVMBuildBr(builder, done_block);
            LLVMGetInsertBlock(builder)
        };

        let llvm_type = self.codegen.llvm_type(ty)?;
        self.position_at(done_block);
        // SAFETY: the phi joins the two predecessors just built, both of
        // `llvm_type` — the analyzer proved the branches share a Kira type.
        let result = unsafe {
            let phi = LLVMBuildPhi(builder, llvm_type, c"sel".as_ptr());
            let mut values = [then_value, else_value];
            let mut blocks = [then_exit, else_exit];
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            phi
        };
        Ok(result)
    }

    /// Lowers `&&`/`||` as branches, evaluating the right operand only when the
    /// left does not already decide the answer.
    ///
    /// `short_circuit_on` is the left value that fixes the result: `true` for
    /// `||`, `false` for `&&`.
    fn lower_short_circuit(
        &mut self,
        lhs: IrExprId,
        rhs: IrExprId,
        short_circuit_on: bool,
    ) -> Result<LLVMValueRef, LlvmError> {
        let left = self.lower_expr(lhs)?;
        let function = self.current_function();
        let rhs_block = self.append_block(function, c"logic.rhs");
        let done_block = self.append_block(function, c"logic.end");
        let builder = self.codegen.builder;

        // SAFETY: both blocks belong to the function being built and `left` is
        // an `i1`; the branch records which block reaches the join.
        let left_exit = unsafe {
            let (on_true, on_false) = if short_circuit_on {
                (done_block, rhs_block)
            } else {
                (rhs_block, done_block)
            };
            LLVMBuildCondBr(builder, left, on_true, on_false);
            LLVMGetInsertBlock(builder)
        };

        self.position_at(rhs_block);
        let right = self.lower_expr(rhs)?;
        // SAFETY: the right operand's block is unterminated; join it to the end.
        let right_exit = unsafe {
            LLVMBuildBr(builder, done_block);
            LLVMGetInsertBlock(builder)
        };

        self.position_at(done_block);
        // SAFETY: the phi joins the two predecessors just built, both `i1`.
        let result = unsafe {
            let phi = LLVMBuildPhi(builder, self.codegen.types.i1, c"logic".as_ptr());
            let short_circuited =
                LLVMConstInt(self.codegen.types.i1, u64::from(short_circuit_on), 0);
            let mut values = [short_circuited, right];
            let mut blocks = [left_exit, right_exit];
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            phi
        };
        Ok(result)
    }
}

/// The integer predicate a comparison operator lowers to, if it is one.
fn integer_predicate(op: IrBinOp) -> Option<LLVMIntPredicate> {
    Some(match op {
        // Booleans are `i1`, so their comparisons are integer comparisons.
        IrBinOp::EqInt | IrBinOp::EqBool => LLVMIntPredicate::LLVMIntEQ,
        IrBinOp::NeInt | IrBinOp::NeBool => LLVMIntPredicate::LLVMIntNE,
        IrBinOp::LtInt => LLVMIntPredicate::LLVMIntSLT,
        IrBinOp::LeInt => LLVMIntPredicate::LLVMIntSLE,
        IrBinOp::GtInt => LLVMIntPredicate::LLVMIntSGT,
        IrBinOp::GeInt => LLVMIntPredicate::LLVMIntSGE,
        // The `U8`..`U64` spellings. Equality needs no unsigned twin: the same
        // 64 bits compare equal under either signedness.
        IrBinOp::LtUInt => LLVMIntPredicate::LLVMIntULT,
        IrBinOp::LeUInt => LLVMIntPredicate::LLVMIntULE,
        IrBinOp::GtUInt => LLVMIntPredicate::LLVMIntUGT,
        IrBinOp::GeUInt => LLVMIntPredicate::LLVMIntUGE,
        _ => return None,
    })
}

/// The float predicate a comparison operator lowers to, if it is one.
///
/// Ordered predicates match Rust's `f64` comparisons, where any comparison with
/// a NaN is false — except `!=`, which is `!(a == b)` and so is true for NaN.
fn real_predicate(op: IrBinOp) -> Option<LLVMRealPredicate> {
    Some(match op {
        IrBinOp::EqFloat => LLVMRealPredicate::LLVMRealOEQ,
        IrBinOp::NeFloat => LLVMRealPredicate::LLVMRealUNE,
        IrBinOp::LtFloat => LLVMRealPredicate::LLVMRealOLT,
        IrBinOp::LeFloat => LLVMRealPredicate::LLVMRealOLE,
        IrBinOp::GtFloat => LLVMRealPredicate::LLVMRealOGT,
        IrBinOp::GeFloat => LLVMRealPredicate::LLVMRealOGE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_comparison_operator_has_exactly_one_predicate() {
        // A comparison must lower through one path or the other; an operator
        // that answers to both (or neither) would silently mis-lower.
        for op in [
            IrBinOp::EqInt,
            IrBinOp::NeInt,
            IrBinOp::LtInt,
            IrBinOp::LeInt,
            IrBinOp::GtInt,
            IrBinOp::GeInt,
            IrBinOp::LtUInt,
            IrBinOp::LeUInt,
            IrBinOp::GtUInt,
            IrBinOp::GeUInt,
            IrBinOp::EqBool,
            IrBinOp::NeBool,
            IrBinOp::EqFloat,
            IrBinOp::NeFloat,
            IrBinOp::LtFloat,
            IrBinOp::LeFloat,
            IrBinOp::GtFloat,
            IrBinOp::GeFloat,
        ] {
            assert_ne!(
                integer_predicate(op).is_some(),
                real_predicate(op).is_some(),
                "{op:?} must lower through exactly one comparison path",
            );
        }
    }

    #[test]
    fn arithmetic_operators_are_not_comparisons() {
        for op in [
            IrBinOp::AddInt,
            IrBinOp::DivInt,
            IrBinOp::AddFloat,
            IrBinOp::ConcatStr,
        ] {
            assert!(integer_predicate(op).is_none() && real_predicate(op).is_none());
        }
    }
}
