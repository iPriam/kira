use super::*;

use kira_ir::{IrExprId, IrPlace};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

impl FunctionLowering<'_, '_> {
    /// Builds an array from its written elements and leaves its handle.
    ///
    /// Allocate full, then fill: `array_new` reserves exactly this many slots,
    /// so each element is written through `array_slot` at a constant, in-range
    /// index — the bounds check the runtime does there can never fire here. The
    /// slots are fresh, so a plain store suffices; there is no prior value to
    /// drop, unlike a store into a live element.
    ///
    /// The read slot rather than the mutable one, even though this writes: the
    /// item block was allocated a few instructions ago and no other array has
    /// ever seen it, so there is nothing for a copy to protect.
    pub(super) fn lower_array_new(
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
    /// The element is copied out — the array owns it, so handing it out
    /// unshared means copying it first — and that copy is what preserves value
    /// semantics. The *base* does not need copying at all.
    ///
    /// # Reading an element does not copy the array
    ///
    /// A general base expression is evaluated, indexed, and dropped. A base
    /// that is just a local is **borrowed** instead: its handle is read without
    /// a clone and never freed here, because this expression does not own it.
    ///
    /// Cloning it would make one element read cost the whole array, so a loop
    /// over `n` elements would cost `O(n²)` — reading 200,000 elements took
    /// seven seconds before this, and loading an 18 MB mesh never finished.
    pub(super) fn lower_index(
        &mut self,
        base: IrExprId,
        index: IrExprId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let Some(handle) = self.borrowed_local_handle(base)? {
            let slot = self.element_slot(handle, index, ty)?;
            return self.read_owned(slot, ty);
        }
        let base_ty = self.type_of(base);
        let base_value = self.lower_expr(base)?;
        let slot = self.element_slot(base_value, index, ty)?;
        let copy = self.read_owned(slot, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    pub(super) fn lower_array_len(&mut self, array: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let array_ty = self.type_of(array);
        // Counting does not consume the array: a local holding one whose
        // elements run a user `Drop` still holds it afterwards, and the share
        // this took is what the release below gives back.
        let array_value = self.lower_borrowed_expr(array)?;
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
    pub(super) fn lower_array_append(
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
