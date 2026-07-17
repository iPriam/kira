//! Expression lowering: constants, local reads, and every operator.
//!
//! The arithmetic here is where native code and the interpreter agree or fail
//! to: wrapping integer ops, a trapping division, and short-circuit operators
//! that are control flow rather than instructions.

use kira_ir::{IrBinOp, IrExpr, IrExprId, IrPlace, IrUnOp};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMRealPredicate};

use super::super::ffi::c_string;
use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers an expression to a value.
    pub(super) fn lower_expr(&mut self, id: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        match self.codegen.program.expr(id).clone() {
            IrExpr::Int(value) => Ok(self.codegen.const_int(value)),
            IrExpr::Float(value) => Ok(self.codegen.const_float(value)),
            IrExpr::Bool(value) => Ok(self.codegen.const_bool(value)),
            IrExpr::Str(text) => {
                let data = self.codegen.string_constant(&text);
                let length = self.codegen.const_int(text.len() as i64);
                Ok(self.call(self.codegen.runtime.str_new, &mut [data, length], c"str"))
            }
            IrExpr::Local(slot) => self.load_local(slot),
            IrExpr::Unary { op, operand } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_unary(op, value))
            }
            IrExpr::Binary { op, lhs, rhs } => self.lower_binary(op, lhs, rhs),
            IrExpr::Call { callee, args, .. } => self.lower_call(callee, &args),
            IrExpr::StructNew { struct_id, fields } => self.lower_struct_new(struct_id, &fields),
            IrExpr::Field { base, index, ty } => self.lower_field(base, index, ty),
            IrExpr::ArrayNew { ty, elements } => self.lower_array_new(ty, &elements),
            IrExpr::Index { base, index, ty } => self.lower_index(base, index, ty),
            IrExpr::ArrayLen { array } => self.lower_array_len(array),
            IrExpr::ArrayAppend { place, value } => self.lower_array_append(&place, value),
        }
    }

    /// Builds an array from its written elements and leaves its handle.
    ///
    /// Allocate full, then fill: `array_new` reserves exactly this many slots,
    /// so each element is written through `array_slot` at a constant, in-range
    /// index — the bounds check the runtime does there can never fire here. The
    /// slots are fresh, so a plain store suffices; there is no prior value to
    /// drop, unlike a store into a live element.
    fn lower_array_new(
        &mut self,
        ty: Type,
        elements: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let element = self.codegen.element_of(ty)?;
        let count = self.codegen.const_int(elements.len() as i64);
        let esize = self.codegen.abi_size(element)?;
        let handle = self.call(
            self.codegen.runtime.array_new,
            &mut [count, esize],
            c"array",
        );
        for (index, &value) in elements.iter().enumerate() {
            // Elements evaluate left to right, as the VM pushes them.
            let lowered = self.lower_expr(value)?;
            let at = self.codegen.const_int(index as i64);
            let esize = self.codegen.abi_size(element)?;
            let slot = self.call(
                self.codegen.runtime.array_slot,
                &mut [handle, at, esize],
                c"slot",
            );
            // SAFETY: `slot` points at a fresh element slot of `element`'s type
            // and `lowered` has that type; the builder is on a live block.
            unsafe { LLVMBuildStore(self.codegen.builder, lowered, slot) };
        }
        Ok(handle)
    }

    /// Reads one element out of an array (`xs[i]`).
    ///
    /// The VM's `ArrayGet`, in the same order: the base is evaluated (a local
    /// read clones the whole array), the element is copied out *before* the base
    /// is dropped — the array owns the element, so handing it out unshared means
    /// copying it first — and then the base clone is freed.
    fn lower_index(
        &mut self,
        base: IrExprId,
        index: IrExprId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let base_ty = self.type_of(base);
        let base_value = self.lower_expr(base)?;
        let slot = self.element_slot(base_value, index, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `slot` points at a live element of `llvm_type`, bounds-checked
        // by the runtime, and the builder is on a live block.
        let element =
            unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, slot, c"elem".as_ptr()) };
        let copy = self.copy_value(element, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// Turns an array handle into the address of element `index`, bounds-checked
    /// by the runtime.
    pub(super) fn element_slot(
        &mut self,
        array: LLVMValueRef,
        index: IrExprId,
        element: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let index_value = self.lower_expr(index)?;
        let esize = self.codegen.abi_size(element)?;
        Ok(self.call(
            self.codegen.runtime.array_slot,
            &mut [array, index_value, esize],
            c"slot",
        ))
    }

    /// An array's element count (`xs.count`), the VM's `ArrayLen`.
    fn lower_array_len(&mut self, array: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let array_ty = self.type_of(array);
        let array_value = self.lower_expr(array)?;
        let len = self.call(self.codegen.runtime.array_len, &mut [array_value], c"len");
        self.drop_value(array_value, array_ty)?;
        Ok(len)
    }

    /// Appends one element to the array a place names (`xs.append(v)`), yielding
    /// `Void`.
    ///
    /// The VM's `ArrayAppend`, in the same order: the place's index expressions
    /// are evaluated first, then the value, and only then is the slot reserved —
    /// so a value that reads the array (`xs.append(xs.count)`) sees the length
    /// from before the push, as the VM's evaluate-then-append order does. The
    /// slot is fresh, so a plain store lands the value with nothing to drop.
    fn lower_array_append(
        &mut self,
        place: &IrPlace,
        value: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        // Every step is a walk: the place names the array itself, and the walk
        // lands on the slot that *holds* its handle.
        let (slot, ty) = self.walk_place(place.local, &place.path)?;
        let element = self.codegen.element_of(ty)?;
        // SAFETY: `slot` holds an array handle (a `ptr`); the builder is live.
        let handle = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                slot,
                c"array".as_ptr(),
            )
        };
        let lowered = self.lower_expr(value)?;
        let esize = self.codegen.abi_size(element)?;
        let element_slot = self.call(
            self.codegen.runtime.array_push_slot,
            &mut [handle, esize],
            c"push",
        );
        // SAFETY: `element_slot` is a fresh, uninitialized element slot of
        // `element`'s type and `lowered` has that type.
        Ok(unsafe { LLVMBuildStore(self.codegen.builder, lowered, element_slot) })
    }

    /// Reads a local slot, copying what it holds.
    ///
    /// The VM's `LoadLocal` copies the value, so the slot keeps ownership of
    /// its own storage and the reader owns an independent copy.
    fn load_local(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.local_type(slot)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        let pointer = self.local_pointer(slot)?;
        let name = c_string(&format!("local.{slot}.read"));
        // SAFETY: `pointer` is this slot's alloca of `llvm_type`.
        let value =
            unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, name.as_ptr()) };
        self.copy_value(value, ty)
    }

    /// Builds a struct value from its fields.
    ///
    /// The fields arrive in declaration order with every one present — analysis
    /// filled the defaults — so this is a straight `insertvalue` chain onto a
    /// zeroed value, with no reordering and no gaps.
    fn lower_struct_new(
        &mut self,
        struct_id: kira_semantics_model::StructId,
        fields: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = Type::Struct(struct_id);
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` is this struct's type in this live context.
        let mut value = unsafe { LLVMGetUndef(llvm_type) };
        for (index, &field) in fields.iter().enumerate() {
            let lowered = self.lower_expr(field)?;
            value = self.insert_field(value, lowered, index as u32)?;
        }
        Ok(value)
    }

    /// Reads one field out of a struct expression.
    ///
    /// The field is copied out *before* the base is dropped, because the base
    /// owns the storage the field names — handing it out without copying would
    /// hand out exactly what the drop is about to free. This is the VM's
    /// `GetField` instruction, in the same order.
    fn lower_field(
        &mut self,
        base: IrExprId,
        index: u32,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let base_ty = self.type_of(base);
        let base_value = self.lower_expr(base)?;
        let field = self.extract_field(base_value, index)?;
        let copy = self.copy_value(field, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// Lowers a unary operator.
    fn lower_unary(&mut self, op: IrUnOp, value: LLVMValueRef) -> LLVMValueRef {
        let builder = self.codegen.builder;
        // SAFETY: `value` has the operand type the typed operator fixes, and
        // the builder is on a live block. `LLVMBuildNeg` carries no `nsw`, so
        // it wraps like the VM's `wrapping_neg`.
        unsafe {
            match op {
                IrUnOp::NegInt => LLVMBuildNeg(builder, value, c"neg".as_ptr()),
                IrUnOp::NegFloat => LLVMBuildFNeg(builder, value, c"fneg".as_ptr()),
                IrUnOp::Not => LLVMBuildNot(builder, value, c"not".as_ptr()),
            }
        }
    }

    /// Lowers a binary operator.
    fn lower_binary(
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
                IrBinOp::AddFloat => LLVMBuildFAdd(builder, left, right, c"fadd".as_ptr()),
                IrBinOp::SubFloat => LLVMBuildFSub(builder, left, right, c"fsub".as_ptr()),
                IrBinOp::MulFloat => LLVMBuildFMul(builder, left, right, c"fmul".as_ptr()),
                IrBinOp::DivFloat => LLVMBuildFDiv(builder, left, right, c"fdiv".as_ptr()),
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
                other => {
                    let predicate = integer_predicate(other);
                    match predicate {
                        Some(predicate) => {
                            LLVMBuildICmp(builder, predicate, left, right, c"icmp".as_ptr())
                        }
                        None => {
                            let predicate = real_predicate(other).ok_or(LlvmError::Unsupported(
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
