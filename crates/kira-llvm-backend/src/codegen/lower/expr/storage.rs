//! Local, struct, and addressable-place expression lowering.

use kira_ir::{IrExpr, IrExprId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Reads a local slot, copying what it holds.
    ///
    /// The VM's `LoadLocal` copies the value, so the slot keeps ownership of
    /// its own storage and the reader owns an independent copy.
    pub(in crate::codegen) fn load_local(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.local_type(slot)?;
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(slot as usize)
            .copied()
            .flatten()
        {
            return self.load_native_state_local(slot, type_id, ty);
        }
        let pointer = self.local_pointer(slot)?;
        let value = self.read_owned(pointer, ty)?;
        // A value that runs a user `Drop` is never copied — binding one moves
        // (`TypeTable::moves_on_bind`), so the checker has already refused a
        // second use of this local. Reading it therefore *takes* it: the local
        // no longer holds anything, and the release at the end of the frame
        // must not run a body the value's new owner will run.
        // Only a user-`Drop` value is moved by an ordinary read. Other
        // heap-backed values are copied by `read_owned`, so their local keeps
        // owning the original share and remains eligible for a later scope or
        // frame release.
        if self
            .local_type(slot)
            .is_ok_and(|ty| self.codegen.program.types.runs_user_drop(ty))
        {
            self.clear_live_flag(slot);
        }
        Ok(value)
    }

    /// Lowers `expr` in a position that does not consume it.
    ///
    /// Only a local read differs: a local whose type runs a user `Drop` is
    /// *taken* by an ordinary read, and a borrowed position leaves the caller
    /// holding the value. Everywhere else the value is a temporary the position
    /// owns either way.
    pub(in crate::codegen) fn lower_borrowed_expr(
        &mut self,
        expr: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let IrExpr::Local(slot) = *self.codegen.program.expr(expr) else {
            return self.lower_expr(expr);
        };
        let value = self.lower_expr(expr)?;
        // A borrowed read of an ordinary heap value leaves its local live; the
        // flag only tracks the move-sensitive user-`Drop` case.
        if self
            .local_type(slot)
            .is_ok_and(|ty| self.codegen.program.types.runs_user_drop(ty))
        {
            self.set_live_flag(slot);
        }
        Ok(value)
    }

    /// Marks a local whose type runs a user `Drop` as holding a value again,
    /// undoing the take an ordinary read performed.
    pub(in crate::codegen) fn set_live_flag(&mut self, slot: u32) {
        let Some(flag) = self.live_flag(slot) else {
            return;
        };
        // SAFETY: `flag` addresses an `i1` in this function's entry block and
        // the builder is on a live block.
        unsafe {
            LLVMBuildStore(
                self.codegen.builder,
                LLVMConstInt(self.codegen.types.i1, 1, 0),
                flag,
            );
        }
    }

    /// Marks a local whose type runs a user `Drop` as no longer holding a
    /// value. A local of any other type has no flag and this does nothing.
    pub(in crate::codegen) fn clear_live_flag(&mut self, slot: u32) {
        let Some(flag) = self.live_flag(slot) else {
            return;
        };
        // SAFETY: `flag` addresses an `i1` in this function's entry block and
        // the builder is on a live block.
        unsafe {
            LLVMBuildStore(
                self.codegen.builder,
                LLVMConstInt(self.codegen.types.i1, 0, 0),
                flag,
            );
        }
    }

    /// Builds a struct value from its fields.
    ///
    /// The fields arrive in declaration order with every one present — analysis
    /// filled the defaults — so this is a straight `insertvalue` chain onto a
    /// zeroed value, with no reordering and no gaps.
    pub(in crate::codegen) fn lower_struct_new(
        &mut self,
        struct_id: kira_semantics_model::StructId,
        fields: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = Type::Struct(struct_id);
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` is this struct's type in this live context.
        let mut value = unsafe { LLVMGetUndef(llvm_type) };
        for (index, &field) in fields.iter().enumerate() {
            let mut lowered = self.lower_expr(field)?;
            if self.c_storage_slot(ty, index)? && self.type_of(field) != Type::CBlock {
                lowered = self.call(
                    self.codegen.runtime.cblock_alien,
                    &mut [lowered],
                    c"struct.cblock.alien",
                );
            }
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
    pub(in crate::codegen) fn lower_field(
        &mut self,
        base: IrExprId,
        index: u32,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        // A base that names storage is read through it. Lowering it as a value
        // first would load the whole struct to reach one field of it, then copy
        // and drop everything else in it — and a generated style struct is
        // thousands of bytes, which at a development build's code-generation
        // level is a move per field, three times over, for one read.
        let base_ty = self.type_of(base);
        if matches!(base_ty, Type::Struct(_))
            && let Some(pointer) = self.addressable(base)?
        {
            let struct_type = self.codegen.llvm_type(base_ty)?;
            let field = self.codegen.field_pointer(struct_type, pointer, index);
            if self.c_storage_slot(base_ty, index as usize)? {
                // SAFETY: an owning C-layout slot contains one live or null
                // i64 C-block handle.
                let handle = unsafe {
                    LLVMBuildLoad2(
                        self.codegen.builder,
                        self.codegen.types.i64,
                        field,
                        c"field.cblock".as_ptr(),
                    )
                };
                return Ok(self.call(
                    self.codegen.runtime.cblock_word,
                    &mut [handle],
                    c"field.cblock.word",
                ));
            }
            return self.read_owned(field, ty);
        }
        // Reading a member does not consume the value it is read from.
        let base_value = self.lower_borrowed_expr(base)?;
        let field = self.extract_field(base_value, index)?;
        if self.c_storage_slot(base_ty, index as usize)? {
            let word = self.call(
                self.codegen.runtime.cblock_word,
                &mut [field],
                c"field.cblock.word",
            );
            self.drop_value(base_value, base_ty)?;
            return Ok(word);
        }
        let copy = self.copy_value(field, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// The storage `expr` names, when it names storage this frame can address.
    ///
    /// A local slot, or a struct field of one, however deeply nested. Nothing
    /// else: an expression that computes a value has no address, and a place
    /// behind an array or an enum is reached through the runtime rather than
    /// through a `getelementptr` on this frame.
    ///
    /// Reading through the address is only sound because nothing between the
    /// walk and the read can write the slot — the walk is a chain of
    /// `getelementptr`, which evaluates nothing.
    pub(in crate::codegen) fn addressable(
        &mut self,
        expr: IrExprId,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        match *self.codegen.program.expr(expr) {
            IrExpr::Local(slot) => {
                // A callback-state local holds a token rather than the value,
                // and a written-through parameter is already a pointer the
                // caller owns; both are read through their own paths.
                if self
                    .function
                    .native_state_locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                    .is_some()
                {
                    return Ok(None);
                }
                Ok(Some(self.local_pointer(slot)?))
            }
            IrExpr::Field { base, index, .. } => {
                let base_ty = self.type_of(base);
                if !matches!(base_ty, Type::Struct(_)) {
                    return Ok(None);
                }
                let Some(pointer) = self.addressable(base)? else {
                    return Ok(None);
                };
                let struct_type = self.codegen.llvm_type(base_ty)?;
                Ok(Some(self.codegen.field_pointer(
                    struct_type,
                    pointer,
                    index,
                )))
            }
            // An element of a borrowable array is storage this frame reaches
            // too: the runtime hands back the element's own slot, and a walk
            // into it is address arithmetic like every other step. Reading
            // through it is what keeps `rows[i].cell.tag` from copying the
            // element out — and a copy of a value that runs a user `Drop` would
            // run its body when the copy died, which is a body the reader never
            // asked for.
            IrExpr::Index {
                base: array,
                index: at,
                ty: element,
            } => {
                let Some(handle) = self.borrowed_local_handle(array)? else {
                    return Ok(None);
                };
                Ok(Some(self.element_slot(handle, at, element)?))
            }
            _ => Ok(None),
        }
    }
}
