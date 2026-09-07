//! Array indexing and array-place storage lowering.

use kira_ir::{IrExpr, IrExprId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::super::ffi::c_string;
use super::super::FunctionLowering;
use crate::LlvmError;

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
    pub(in crate::codegen) fn lower_array_new(
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
    pub(in crate::codegen) fn lower_index(
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

    /// Reads a value of `ty` out of `slot`, taking a share of what it owns.
    ///
    /// The share is taken **through the slot**, before the read: a copy raises
    /// counts and changes no bits, so the value loaded afterwards is the copy.
    /// Retaining first is what keeps a large struct to a single load — spilling
    /// the loaded value back into a scratch slot for the walk would double it.
    pub(in crate::codegen) fn read_owned(
        &mut self,
        slot: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if self.codegen.owns_unique_c_storage(ty) {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `slot` addresses a live value of `llvm_type`.
            let value = unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    llvm_type,
                    slot,
                    c"owned.source".as_ptr(),
                )
            };
            return self.copy_value(value, ty);
        }
        self.codegen.retain_at(slot, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `slot` addresses a live value of `llvm_type` and the builder
        // is on a live block.
        Ok(unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, slot, c"owned".as_ptr()) })
    }

    /// The handle a place holds, read without copying what holds it.
    ///
    /// `None` when the expression is not a place this can address, and the
    /// general route — evaluate, use, drop — handles it.
    pub(in crate::codegen) fn borrowed_local_handle(
        &mut self,
        base: IrExprId,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let Some((pointer, ty)) = self.borrowed_place_pointer(base)? else {
            return Ok(None);
        };
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `pointer` addresses a live value of `llvm_type`; the handle is
        // read, not copied, and is not freed here because this expression does
        // not own it.
        Ok(Some(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                llvm_type,
                pointer,
                c"place.borrow".as_ptr(),
            )
        }))
    }

    /// The address a place expression names, and the type stored there.
    ///
    /// A place is anything reached by naming a local and walking into it —
    /// `xs`, `tree.nodes`, `rows[i].cells`. Every step of that walk is address
    /// arithmetic, so the whole path can be addressed rather than evaluated,
    /// and a handle at the end of it read without cloning what holds it.
    ///
    /// This lets `tree.nodes[i]` address the array in place instead of cloning
    /// every element before reading one entry.
    ///
    /// `None` for anything that is not such a place: the caller then evaluates
    /// the expression, uses it, and drops it as before.
    pub(in crate::codegen) fn borrowed_place_pointer(
        &mut self,
        expr: IrExprId,
    ) -> Result<Option<(LLVMValueRef, Type)>, LlvmError> {
        match *self.codegen.program.expr(expr) {
            IrExpr::Local(slot) => {
                let ty = self.local_type(slot)?;
                // A native-state local holds a token, and the value it names
                // lives in the box behind it — which is addressable, so a place
                // rooted here is addressed through the box's payload.
                if let Some(type_id) = self
                    .function
                    .native_state_locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                {
                    if !self.state_is_boxed() {
                        return Ok(None);
                    }
                    let payload = self.recover_native_state_alloca(slot, type_id, ty)?.0;
                    return Ok(Some((payload, ty)));
                }
                Ok(Some((self.local_pointer(slot)?, ty)))
            }
            // A constant's global is immutable after initialization, so a
            // borrow reads it where it sits. The hybrid native half has no
            // global and falls back to the evaluated copy.
            IrExpr::ConstantGet { constant, ty } => Ok(self
                .codegen
                .constant_global(constant)
                .map(|global| (global, ty))),
            IrExpr::Field { base, index, ty } => {
                let Some((pointer, base_ty)) = self.borrowed_place_pointer(base)? else {
                    return Ok(None);
                };
                if !matches!(base_ty, Type::Struct(_)) {
                    return Ok(None);
                }
                let struct_type = self.codegen.llvm_type(base_ty)?;
                let name = c_string(&format!("place.field.{index}.ptr"));
                // SAFETY: `pointer` addresses a value of `struct_type`, and
                // `index` came from that struct's own definition.
                let field = unsafe {
                    LLVMBuildStructGEP2(
                        self.codegen.builder,
                        struct_type,
                        pointer,
                        index,
                        name.as_ptr(),
                    )
                };
                Ok(Some((field, ty)))
            }
            IrExpr::Index { base, index, ty } => {
                let Some(handle) = self.borrowed_local_handle(base)? else {
                    return Ok(None);
                };
                Ok(Some((self.element_slot(handle, index, ty)?, ty)))
            }
            _ => Ok(None),
        }
    }

    /// Turns an array handle into the address of element `index` **to read**,
    /// bounds-checked by the runtime.
    ///
    /// The item block behind the handle may be shared with another array, so
    /// nothing may be written through the address this gives back;
    /// [`Self::element_slot_mut`] is the one that may.
    pub(in crate::codegen) fn element_slot(
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

    /// Turns the **slot holding** an array into the address of element `index`
    /// to write, bounds-checked by the runtime.
    ///
    /// Copying an array only takes a share of it, so a write is where the
    /// copying actually happens: the runtime gives this slot an array of its
    /// own first, cloning each element with the leaf handed over here, and
    /// stores the fresh handle back. That is why this takes the slot rather
    /// than the handle — a split *replaces* the handle, and whatever holds it
    /// has to see that.
    ///
    /// Every write into an array goes through this — a store, an append, and a
    /// step of a place walk that passes through one — and each of them already
    /// starts from a place, so the slot costs nothing to supply.
    pub(in crate::codegen) fn element_slot_mut(
        &mut self,
        holder: LLVMValueRef,
        index: IrExprId,
        element: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let index_value = self.lower_expr(index)?;
        let esize = self.codegen.abi_size(element)?;
        let clone = self.codegen.element_clone(element)?;
        Ok(self.call(
            self.codegen.runtime.array_slot_mut,
            &mut [holder, index_value, esize, clone],
            c"slot.mut",
        ))
    }
}
