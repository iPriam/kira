//! Lowering the three capture-cell primitives.
//!
//! A cell is the shared, mutable storage a `var` moves into when a closure
//! captures it. The box is `kira-native-bridge`'s enum box with the tag unused,
//! so copying and releasing a cell are the share bump and the release
//! [`super::super::values`] already emits — nothing here does ownership. What
//! *is* here is the three operations an enum never needs, because an enum is
//! never written through: making the box, reading it, and replacing what it
//! holds.
//!
//! # Why the get and set forms name a slot
//!
//! Every cell the compiler mints lives in a local, so a read borrows the handle
//! out of the slot instead of taking a share of it and dropping it again. The
//! slot keeps its hold throughout.
//!
//! # Why a wide value goes out of line
//!
//! A struct is wider than the box's payload word, and an array handle needs
//! type-specific clone and free leaves. Both take the runtime's erased
//! aggregate payload, exactly as a struct enum payload does.

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

/// Whether a value of `ty` travels in the box's payload word or out of line.
///
/// The same line [`super::super::boxing`] draws for an enum payload, and drawn
/// once here so a cell's construction, read, and write cannot disagree about it.
fn is_out_of_line(ty: Type) -> bool {
    matches!(ty, Type::Struct(_) | Type::Array(_))
}

impl FunctionLowering<'_, '_> {
    /// `CellNew`: box a value into a fresh cell holding one hold.
    ///
    /// The value moves in: whatever it owned is the box's, which is what makes
    /// the box's release reclaim it.
    pub(in crate::codegen) fn lower_cell_new(
        &mut self,
        value: kira_ir::IrExprId,
        cell_ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let inner = self.cell_inner(cell_ty)?;
        let lowered = self.lower_expr(value)?;
        if is_out_of_line(inner) {
            let (slot, size, clone, free) = self.spill_for_cell(lowered, inner)?;
            return Ok(self.call(
                self.codegen.runtime.cell_new_aggregate,
                &mut [slot, size, clone, free],
                c"cell",
            ));
        }
        let (kind, word) = self.codegen.encode_box_payload(inner, lowered)?;
        Ok(self.call(self.codegen.runtime.cell_new, &mut [kind, word], c"cell"))
    }

    /// `CellGet`: read an **owned** copy of what the cell in `slot` holds.
    ///
    /// The slot is borrowed, never consumed — nothing is dropped here — and the
    /// value that comes back is the caller's. A borrowing read would let a
    /// write through another holder free the payload while this caller had it,
    /// and a cell exists precisely so that other holders exist.
    pub(in crate::codegen) fn lower_cell_get(
        &mut self,
        slot: u32,
        inner: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let handle = self.load_cell_handle(slot)?;
        if is_out_of_line(inner) {
            let llvm_type = self.codegen.llvm_type(inner)?;
            // SAFETY: `llvm_type` belongs to this context, and the runtime
            // writes one owned value of exactly that type into `out`.
            let out = unsafe {
                LLVMBuildAlloca(self.codegen.builder, llvm_type, c"cell.payload".as_ptr())
            };
            self.call(
                self.codegen.runtime.cell_get_aggregate,
                &mut [handle, out],
                c"",
            );
            // SAFETY: the helper initialized `out` with a value of `llvm_type`.
            return Ok(unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    llvm_type,
                    out,
                    c"cell.payload.value".as_ptr(),
                )
            });
        }
        let word = self.call(self.codegen.runtime.cell_get, &mut [handle], c"cell.word");
        self.codegen.decode_box_payload(inner, word)
    }

    /// `CellSet`: replace what the cell in `slot` holds, in one call.
    ///
    /// One runtime call, never a release followed by a store: a split path
    /// leaves the box holding a freed handle for the window between the two,
    /// and a trap in that window leaves it there for good. Nothing is handed a
    /// pointer into the payload slot either.
    pub(in crate::codegen) fn lower_cell_set(
        &mut self,
        slot: u32,
        value: kira_ir::IrExprId,
    ) -> Result<(), LlvmError> {
        let inner = self.type_of(value);
        // The value is computed before the handle is read, matching the VM,
        // where `CellSet`'s operand is pushed before the instruction runs.
        let lowered = self.lower_expr(value)?;
        let handle = self.load_cell_handle(slot)?;
        if is_out_of_line(inner) {
            let (source, size, clone, free) = self.spill_for_cell(lowered, inner)?;
            self.call(
                self.codegen.runtime.cell_set_aggregate,
                &mut [handle, source, size, clone, free],
                c"",
            );
            return Ok(());
        }
        let (kind, word) = self.codegen.encode_box_payload(inner, lowered)?;
        self.call(
            self.codegen.runtime.cell_set,
            &mut [handle, kind, word],
            c"",
        );
        Ok(())
    }

    /// The cell handle a slot holds, borrowed rather than copied.
    ///
    /// No share is taken: the slot keeps its hold for as long as it is live,
    /// and every use here finishes before it can be released.
    fn load_cell_handle(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let pointer = self.local_pointer(slot)?;
        // SAFETY: `pointer` is this slot's alloca, and a cell slot's LLVM type
        // is the opaque pointer every runtime handle is.
        Ok(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                pointer,
                c"cell.handle".as_ptr(),
            )
        })
    }

    /// Spills a wide value to stack storage the runtime can move out of.
    ///
    /// Gives back `(source, size, clone, free)` — the four arguments both
    /// aggregate helpers take. Ownership of the value passes to the runtime,
    /// which is why the leaves are the *element* leaves: the same pair an array
    /// of this type would hand its own helpers.
    fn spill_for_cell(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(LLVMValueRef, LLVMValueRef, LLVMValueRef, LLVMValueRef), LlvmError> {
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this context, `value` has that type,
        // and the builder is positioned on a live block.
        let slot = unsafe {
            let slot = LLVMBuildAlloca(self.codegen.builder, llvm_type, c"cell.source".as_ptr());
            LLVMBuildStore(self.codegen.builder, value, slot);
            slot
        };
        let size = self.codegen.abi_size(ty)?;
        let clone = self.codegen.element_clone(ty)?;
        let free = self.codegen.element_free(ty)?;
        Ok((slot, size, clone, free))
    }

    /// The type a cell type holds.
    fn cell_inner(&self, ty: Type) -> Result<Type, LlvmError> {
        self.codegen
            .program
            .types
            .cell_inner(ty)
            .ok_or(LlvmError::Unsupported("a capture cell of no type"))
    }
}
