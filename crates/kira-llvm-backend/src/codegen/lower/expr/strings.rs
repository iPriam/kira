//! Task, string, and array append expression lowering.

use kira_ir::{IrExprId, IrPlace};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// An array's element count (`xs.count`), the VM's `ArrayLen`.
    /// One deferred-task primitive: the native mirror of the VM's `TaskOp`.
    ///
    /// The operands are lowered left to right, which is the order the VM pushes
    /// them, so a program whose arguments have side effects orders those side
    /// effects identically on both engines.
    pub(in crate::codegen) fn lower_task_op(
        &mut self,
        prim: kira_runtime_abi::TaskPrim,
        operands: [IrExprId; 3],
    ) -> Result<LLVMValueRef, LlvmError> {
        let tag = self.codegen.const_int(i64::from(prim.as_byte()));
        let first = self.lower_expr(operands[0])?;
        let second = self.lower_expr(operands[1])?;
        let third = self.lower_expr(operands[2])?;
        Ok(self.call(
            self.codegen.runtime.task_op,
            &mut [tag, first, second, third],
            c"task",
        ))
    }

    pub(in crate::codegen) fn lower_array_len(
        &mut self,
        array: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let array_ty = self.type_of(array);
        // Counting does not consume the array: a local holding one whose
        // elements run a user `Drop` still holds it afterwards, and the share
        // this took is what the release below gives back.
        let array_value = self.lower_borrowed_expr(array)?;
        let len = self.call(self.codegen.runtime.array_len, &mut [array_value], c"len");
        self.drop_value(array_value, array_ty)?;
        Ok(len)
    }

    /// A string's character count (`s.count`), the VM's `StringLen`.
    ///
    /// The helper consumes the string, which is the lowering convention for
    /// every operation that reads one — so there is nothing to drop here.
    pub(in crate::codegen) fn lower_string_len(
        &mut self,
        text: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        Ok(self.call(self.codegen.runtime.str_count, &mut [value], c"s.count"))
    }

    /// The byte at an index of a string (`s.charAt(i)`), the VM's
    /// `StringCharAt`.
    ///
    /// The helper consumes the string, which is the lowering convention for
    /// every operation that reads one, and traps on an out-of-range index
    /// rather than answering — the same trap the VM raises.
    pub(in crate::codegen) fn lower_string_char_at(
        &mut self,
        text: IrExprId,
        index: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let at = self.lower_expr(index)?;
        Ok(self.call(
            self.codegen.runtime.str_char_at,
            &mut [value, at],
            c"s.charAt",
        ))
    }

    /// A half-open byte slice of a string (`s.substring(a, b)`), the VM's
    /// `StringSubstring`.
    pub(in crate::codegen) fn lower_string_substring(
        &mut self,
        text: IrExprId,
        start: IrExprId,
        end: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let from = self.lower_expr(start)?;
        let to = self.lower_expr(end)?;
        Ok(self.call(
            self.codegen.runtime.str_substring,
            &mut [value, from, to],
            c"s.substring",
        ))
    }

    /// The first byte index of a needle (`s.indexOf(n)`), the VM's
    /// `StringIndexOf`.
    pub(in crate::codegen) fn lower_string_index_of(
        &mut self,
        text: IrExprId,
        needle: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let pattern = self.lower_expr(needle)?;
        Ok(self.call(
            self.codegen.runtime.str_index_of,
            &mut [value, pattern],
            c"s.indexOf",
        ))
    }

    /// One of the shared-opcode string operations, the VM's `StringOp`.
    ///
    /// The operand byte indexes the callable table, so a new operation needs a
    /// row there and nothing here — the receiver and arguments are pushed in
    /// source order whatever the operation is, and each helper frees every
    /// handle it was given.
    pub(in crate::codegen) fn lower_string_operation(
        &mut self,
        op: kira_runtime_abi::StringOp,
        text: IrExprId,
        arguments: Vec<IrExprId>,
    ) -> Result<LLVMValueRef, LlvmError> {
        let mut operands = Vec::with_capacity(arguments.len() + 1);
        operands.push(self.lower_expr(text)?);
        for argument in arguments {
            operands.push(self.lower_expr(argument)?);
        }
        let callable = self.codegen.runtime.string_ops[usize::from(op.as_byte())];
        Ok(self.call(callable, &mut operands, c"s.stringOp"))
    }

    /// A scalar rendered as text (`String(x)`), the VM's `StringOf`.
    ///
    /// The operand's static type picks the helper, so each one formats a value
    /// it already knows the shape of — which is what keeps the rendering
    /// byte-identical to the one `print` gives on this backend.
    pub(in crate::codegen) fn lower_string_of(
        &mut self,
        value: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.type_of(value);
        let operand = self.lower_expr(value)?;
        let callable = match ty {
            Type::Bool => self.codegen.runtime.str_of_bool,
            Type::Float(_) => self.codegen.runtime.str_of_float,
            Type::String => return Ok(operand),
            _ => self.codegen.runtime.str_of_int,
        };
        Ok(self.call(callable, &mut [operand], c"String"))
    }

    /// Appends one element to the array a place names (`xs.append(v)`), yielding
    /// `Void`.
    ///
    /// The VM's `ArrayAppend`, in the same order: the place's index expressions
    /// are evaluated first, then the value, and only then is the slot reserved —
    /// so a value that reads the array (`xs.append(xs.count)`) sees the length
    /// from before the push, as the VM's evaluate-then-append order does. The
    /// slot is fresh, so a plain store lands the value with nothing to drop.
    pub(in crate::codegen) fn lower_array_append(
        &mut self,
        place: &IrPlace,
        value: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(place.local as usize)
            .copied()
            .flatten()
        {
            let root_ty = self.local_type(place.local)?;
            // For a boxed state the array being appended to lives in the
            // state's own storage, so the push reaches it directly.
            let (root, write_back) =
                self.recover_native_state_alloca(place.local, type_id, root_ty)?;
            let mut slot = root;
            let mut ty = root_ty;
            for step in &place.path {
                (slot, ty) = self.walk_place_step(slot, ty, step)?;
            }
            let element = self.codegen.element_of(ty)?;
            let lowered = self.lower_expr(value)?;
            let esize = self.codegen.abi_size(element)?;
            let clone = self.codegen.element_clone(element)?;
            // The slot, not the handle it holds: an append is a write, and a
            // write may split a shared array and leave the slot holding the
            // fresh one.
            let element_slot = self.call(
                self.codegen.runtime.array_push_slot,
                &mut [slot, esize, clone],
                c"push",
            );
            // SAFETY: `element_slot` is one fresh element slot.
            unsafe { LLVMBuildStore(self.codegen.builder, lowered, element_slot) };
            if write_back {
                self.write_back_native_state(place.local, type_id, root_ty, root)?;
            }
            return Ok(self.codegen.const_bool(false));
        }
        // Every step is a walk: the place names the array itself, and the walk
        // lands on the slot that *holds* its handle.
        let (slot, ty) = self.walk_place(place.local, &place.path)?;
        let element = self.codegen.element_of(ty)?;
        let lowered = self.lower_expr(value)?;
        let esize = self.codegen.abi_size(element)?;
        // Appending is a write, so the runtime gives this slot an array of its
        // own — with the leaf cloning each element it carries over — before the
        // new element lands in it. The slot goes over rather than the handle,
        // because a split replaces the handle.
        let clone = self.codegen.element_clone(element)?;
        let element_slot = self.call(
            self.codegen.runtime.array_push_slot,
            &mut [slot, esize, clone],
            c"push",
        );
        // SAFETY: `element_slot` is a fresh, uninitialized element slot of
        // `element`'s type and `lowered` has that type.
        Ok(unsafe { LLVMBuildStore(self.codegen.builder, lowered, element_slot) })
    }
}
