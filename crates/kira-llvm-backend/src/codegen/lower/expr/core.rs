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
            IrExpr::Unary { op, operand, ty } => {
                let value = self.lower_expr(operand)?;
                self.lower_unary_typed(op, value, ty)
            }
            IrExpr::Binary { op, lhs, rhs, ty } => self.lower_binary(op, lhs, rhs, ty),
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
            IrExpr::StructNew {
                struct_id,
                fields,
                order,
            } => self.lower_struct_new(struct_id, &fields, &order),
            IrExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => self.lower_enum_new(enum_id, tag, payload),
            IrExpr::IntoAny { value, from, tag } => self.lower_into_any(value, from, tag),
            IrExpr::TypeConst { value, id } => self.lower_type_const(value, id),
            IrExpr::TypeOf { value } => self.lower_type_of(value),
            IrExpr::TypeCastResult {
                value,
                target,
                result,
                failure,
                payload,
            } => self.lower_type_cast_result(value, target, result, failure, payload),
            IrExpr::TypeField {
                descriptor, field, ..
            } => self.lower_type_field(descriptor, field),
            IrExpr::EnumTag { value } => self.lower_enum_tag(value),
            IrExpr::TypeTest { value, target } => self.lower_type_test(value, target),
            IrExpr::TypeCast { value, target, ty } => self.lower_type_cast(value, target, ty),
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
            IrExpr::MainThreadCall {
                operation,
                function,
                args,
                ty,
            } => self.lower_main_thread_call(operation, function, &args, ty),
            IrExpr::MainThreadJoin { handle, ty } => self.lower_main_thread_join(handle, ty),
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
            IrExpr::NativeUserData { state } => self.lower_native_user_data(state),
            IrExpr::NativeRecover { raw, type_id, ty } => {
                self.lower_native_recover_value(raw, type_id, ty)
            }
            IrExpr::NativeStateRetain { token } => self.lower_native_state_retain(token),
            IrExpr::NativeStateRelease { token } => self.lower_native_state_release(token),
            IrExpr::Convert { operand, kind, ty } => {
                let from = self.type_of(operand);
                let value = self.lower_expr(operand)?;
                self.lower_convert_typed(kind, value, from, ty)
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
