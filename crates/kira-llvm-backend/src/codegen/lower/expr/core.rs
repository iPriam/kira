//! Dispatch for expression lowering.

use kira_ir::{IrExpr, IrExprId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers an expression to a value.
    pub(in crate::codegen) fn lower_expr(
        &mut self,
        id: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        match self.codegen.program.expr(id).clone() {
            IrExpr::Int(value) => Ok(self.codegen.const_int(value)),
            IrExpr::Float(value) => Ok(self.codegen.const_float(value)),
            IrExpr::Bool(value) => Ok(self.codegen.const_bool(value)),
            IrExpr::Str(text) => {
                let data = self.codegen.string_constant(&text);
                // The length is a `usize` at the target's width, not the host's.
                let length = self.codegen.const_usize(text.len() as u64);
                Ok(self.call(self.codegen.runtime.str_new, &mut [data, length], c"str"))
            }
            // A `RawPtr` is an `i64` here, so the null pointer is that zero.
            IrExpr::RawPtrNull => Ok(self.codegen.const_int(0)),
            IrExpr::ForeignCallbackPtr { callback } => {
                self.codegen.callback_thunk_address(callback as usize)
            }
            IrExpr::CellNull { .. } => {
                // A null cell is only closure-representation padding.
                // SAFETY: the pointer type belongs to this live module context.
                Ok(unsafe { LLVMConstNull(self.codegen.types.ptr) })
            }
            IrExpr::Local(slot) => self.load_local(slot),
            IrExpr::ConstantGet { constant, ty } => self.lower_constant_get(constant, ty),
            IrExpr::CellNew { value, ty } => self.lower_cell_new(value, ty),
            IrExpr::CellGet { slot, ty } => self.lower_cell_get(slot, ty),
            IrExpr::Unary { op, operand } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_unary(op, value))
            }
            IrExpr::Binary { op, lhs, rhs } => self.lower_binary(op, lhs, rhs),
            IrExpr::Select {
                cond,
                then,
                otherwise,
                ty,
            } => self.lower_select(cond, then, otherwise, ty),
            IrExpr::Call {
                callee,
                args,
                writebacks,
                result,
                ..
            } => self.lower_call(callee, &args, &writebacks, result),
            IrExpr::StructNew { struct_id, fields } => self.lower_struct_new(struct_id, &fields),
            IrExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => self.lower_enum_new(enum_id, tag, payload),
            IrExpr::IntoAny { value, from } => self.lower_into_any(value, from),
            IrExpr::Widen { value, from, to } => self.lower_widen(value, from, to),
            IrExpr::EnumTag { value } => self.lower_enum_tag(value),
            IrExpr::EnumPayload { value, ty } => self.lower_enum_payload(value, ty),
            IrExpr::Field { base, index, ty } => self.lower_field(base, index, ty),
            IrExpr::MathOperation { op, operands } => self.lower_math_operation(op, &operands),
            IrExpr::ScalarText { value } => self.lower_scalar_text(value),
            IrExpr::ArrayElements { value, element } => self.lower_array_elements(value, element),
            IrExpr::ForeignField {
                base,
                aggregate,
                member,
                ty,
            } => self.lower_foreign_field(base, aggregate, member, ty),
            IrExpr::ForeignMemberAddress {
                base,
                aggregate,
                member,
                ..
            } => self.lower_foreign_member_address(base, aggregate, member),
            IrExpr::ForeignElement {
                base,
                aggregate,
                index,
                ..
            } => self.lower_foreign_element(base, aggregate, index),
            IrExpr::ArrayNew { ty, elements } => self.lower_array_new(ty, &elements),
            IrExpr::Index { base, index, ty } => self.lower_index(base, index, ty),
            IrExpr::TaskOp { prim, operands } => self.lower_task_op(prim, operands),
            IrExpr::ArrayLen { array } => self.lower_array_len(array),
            IrExpr::StringLen { text } => self.lower_string_len(text),
            IrExpr::StringCharAt { text, index } => self.lower_string_char_at(text, index),
            IrExpr::StringSubstring { text, start, end } => {
                self.lower_string_substring(text, start, end)
            }
            IrExpr::StringIndexOf { text, needle } => self.lower_string_index_of(text, needle),
            IrExpr::StringOperation {
                op,
                text,
                ref arguments,
                ..
            } => self.lower_string_operation(op, text, arguments.clone()),
            IrExpr::StringOf { value } => self.lower_string_of(value),
            IrExpr::CLayoutAddress { value, aggregate } => {
                self.lower_clayout_address(value, aggregate)
            }
            IrExpr::CStringNew { text } => {
                let value = self.lower_expr(text)?;
                Ok(self.call(
                    self.codegen.runtime.cblock_text,
                    &mut [value],
                    c"cblock.text",
                ))
            }
            IrExpr::FileSystem { op, args, ty } => self.lower_file_system(op, &args, ty),
            IrExpr::Compiler { op, args, ty } => self.lower_compiler(op, &args, ty),
            IrExpr::Env { op, args, .. } => self.lower_env(op, &args),
            IrExpr::ArrayAppend { place, value } => self.lower_array_append(&place, value),
            IrExpr::NativeState { value, type_id, .. } => {
                self.lower_native_state_new(value, type_id)
            }
            IrExpr::NativeUserData { state } => self.lower_expr(state),
            IrExpr::NativeRecover { raw, type_id, ty } => {
                self.lower_native_recover_value(raw, type_id, ty)
            }
            IrExpr::NativeStateFree { token } => self.lower_native_state_free(token),
            IrExpr::Convert { operand, kind, .. } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_convert(kind, value))
            }
        }
    }

    /// Reads a module constant from its native global, or calls its initializer
    /// across the hybrid seam when this half has no global storage.
    fn lower_constant_get(&mut self, constant: u32, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        if let Some(global) = self.codegen.constant_global(constant) {
            return self.read_owned(global, ty);
        }
        let init = self
            .codegen
            .program
            .constants
            .get(constant as usize)
            .ok_or(LlvmError::internal("a constant read past the table"))?
            .init;
        self.lower_runtime_call(init, &[], &[])
    }
}
