//! Copying and dropping a value as a *call*, not as an expansion.
//!
//! A copy or a drop is a recursive walk of a type: every heap-owning field of
//! every struct it reaches contributes a share-count test, a branch, and a
//! release. Emitting that walk at the site multiplies it by the number of
//! sites, and a generated UI program has hundreds of thousands of them — the
//! walk stops being part of the program and becomes the program.
//!
//! So the walk is emitted **once per type** instead, into the leaves
//! [`super::elements`] already emits for an array's runtime helpers, and every
//! site becomes a call. The leaves are what recursion runs through, so a
//! struct's leaf calls its fields' leaves rather than inlining them, and the
//! whole module carries one walk per distinct type reached rather than one per
//! site.
//!
//! [`super::values`] still holds the walks themselves; this is only where they
//! are entered from.
//!
//! # The scratch slot
//!
//! A leaf walks its value through a pointer, which is what keeps it off the
//! per-field loads a large struct would otherwise cost. A site that has only
//! the value — not an address for it — spills it into one slot per `(function,
//! type)`: the store and the call are emitted with nothing between them, so no
//! two uses of a slot are ever live at once. The slot is allocated in the entry
//! block like every other, so a copy inside a loop does not grow the frame per
//! iteration.

use std::collections::HashMap;

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::elements::Leaf;
use crate::LlvmError;

/// One `(function, type)` scratch allocation.
///
/// The function is keyed by address rather than by value because an
/// `LLVMValueRef` is a raw pointer with no `Hash`; the module owns every
/// function for as long as this map lives, so no address is ever reused.
type ScratchKey = (usize, Type);

/// The slots a site spills a value into for its leaf, one per `(function,
/// type)`.
pub(super) type ScratchSlots = HashMap<ScratchKey, LLVMValueRef>;

impl Codegen<'_> {
    /// Takes a share of everything the value at `at` owns.
    ///
    /// The walk is in [`Codegen::retain_at_walk`]; this is the call to the one
    /// place it was emitted for this type. A value that owns no heap storage
    /// needs neither, and emits nothing at all.
    pub(super) fn retain_at(&mut self, at: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        let retain = self.leaf_callable(ty, Leaf::Retain)?;
        let mut args = [at];
        self.call(retain, &mut args, c"");
        Ok(())
    }

    /// Releases whatever the value at `at` owns.
    ///
    /// [`Codegen::retain_at`]'s counterpart, outlined for the same reason.
    pub(super) fn release_at(&mut self, at: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        let free = self.leaf_callable(ty, Leaf::Free)?;
        let mut args = [at];
        self.call(free, &mut args, c"");
        Ok(())
    }

    /// Produces an independent copy of `value`, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// Shared objects keep the same bits after their share count rises. Unique
    /// C blocks are replaced in the scratch copy with deep-cloned handles, so a
    /// value containing them is loaded back from the walked scratch slot.
    ///
    /// A site that already holds the value in memory should call
    /// [`Codegen::retain_at`] instead: a large struct spills field by field at
    /// the code-generation level a development build uses, and the whole point
    /// of the walk being by pointer is not to pay that.
    pub(super) fn copy_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(value);
        }
        let llvm_type = self.llvm_type(ty)?;
        let source = self.scratch(ty, llvm_type);
        // SAFETY: `source` addresses a slot of `llvm_type` and `value` has that
        // type; the builder is on a live block.
        unsafe { LLVMBuildStore(self.builder, value, source) };
        self.retain_at(source, ty)?;
        if !self.owns_unique_c_storage(ty) {
            return Ok(value);
        }
        // SAFETY: `source` still holds the walked copy of `llvm_type`.
        Ok(unsafe { LLVMBuildLoad2(self.builder, llvm_type, source, c"glue.copy".as_ptr()) })
    }

    /// Releases whatever heap storage `value` owns.
    ///
    /// [`Codegen::copy_value`]'s counterpart, spilling for the same reason and
    /// with the same advice: a site holding a pointer wants
    /// [`Codegen::release_at`].
    pub(super) fn drop_value(&mut self, value: LLVMValueRef, ty: Type) -> Result<(), LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        let llvm_type = self.llvm_type(ty)?;
        let source = self.scratch(ty, llvm_type);
        // SAFETY: `source` addresses a slot of `llvm_type` and `value` has that
        // type; the builder is on a live block.
        unsafe { LLVMBuildStore(self.builder, value, source) };
        self.release_at(source, ty)
    }

    /// Reclaims a materialized recovered-state snapshot without treating its
    /// root as a newly owned `Drop` value.
    pub(super) fn drop_recovered_root_snapshot(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        let llvm_type = self.llvm_type(ty)?;
        let source = self.scratch(ty, llvm_type);
        // SAFETY: `source` addresses a slot of `llvm_type` and `value` has that
        // type; the builder is on a live block.
        unsafe { LLVMBuildStore(self.builder, value, source) };
        self.release_recovered_root_snapshot(source, ty)
    }

    /// Whether the values at `left` and `right` are structurally equal, as an
    /// `i1`.
    ///
    /// A struct's comparison is the same recursive walk a copy and a drop are,
    /// and explodes the same way — the whole of a nested style tree compared
    /// field by field, at every site that compares one. So a struct goes through
    /// its equality leaf, and everything else stays where it is: a scalar
    /// compares in one instruction, and a string, array, or enum in one call to
    /// the runtime, neither of which a leaf would improve on.
    pub(super) fn equal_at(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !matches!(ty, Type::Struct(_)) {
            return self.equal_at_walk(left, right, ty);
        }
        let equals = self.leaf_callable(ty, Leaf::Eq)?;
        let mut args = [left, right];
        let equal = self.call(equals, &mut args, c"eq.struct");
        Ok(self.truthy(equal))
    }

    /// Structural `==` / `!=` on two owned values of one statically known type,
    /// as an `i1` — the backing of `EqValue`/`NeValue`.
    ///
    /// [`equal_at`] compares values where they lie, so each operand is spilled
    /// to a slot first; the walk itself reads and takes nothing. The operator
    /// consumes what it compared — the two operands are owned temporaries — so
    /// both are released afterwards, exactly as the erased `EqAny` drops the
    /// boxes it was handed. `negate` builds the `!=` answer without a second
    /// walk.
    ///
    /// [`equal_at`]: Self::equal_at
    pub(super) fn equal_values(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
        negate: bool,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: a comparison is only lowered inside a function body, so the
        // builder is positioned in one and has a parent to allocate against.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let left_slot = self.entry_alloca(function, llvm_type, c"eq.lhs");
        let right_slot = self.entry_alloca(function, llvm_type, c"eq.rhs");
        // SAFETY: both slots address storage of `llvm_type`, both operands have
        // that type, and the builder is on a live block.
        unsafe {
            LLVMBuildStore(self.builder, left, left_slot);
            LLVMBuildStore(self.builder, right, right_slot);
        }
        let equal = self.equal_at(left_slot, right_slot, ty)?;
        self.release_at(left_slot, ty)?;
        self.release_at(right_slot, ty)?;
        if !negate {
            return Ok(equal);
        }
        // SAFETY: `equal` is an `i1` and the builder is on a live block; `not`
        // on a one-bit value is its logical negation.
        Ok(unsafe { LLVMBuildNot(self.builder, equal, c"ne.value".as_ptr()) })
    }

    /// The three-way order of the values at `left` and `right`, as an `i8` sign
    /// — the ordering twin of [`equal_at`].
    ///
    /// A struct goes through its compare leaf so a nested comparison is a call
    /// rather than an inlined walk, exactly as equality does; everything else
    /// walks where it lies. Neither operand is consumed.
    ///
    /// [`equal_at`]: Self::equal_at
    pub(super) fn compare_at(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !matches!(ty, Type::Struct(_)) {
            return self.compare_at_walk(left, right, ty);
        }
        let compare = self.leaf_callable(ty, Leaf::Cmp)?;
        let mut args = [left, right];
        Ok(self.call(compare, &mut args, c"cmp.struct"))
    }

    /// Structural `<` / `<=` / `>` / `>=` on two owned values of one statically
    /// known `Ordered` type, as the `i8` sign backing [`CmpValue`].
    ///
    /// The ordering counterpart of [`equal_values`]: each operand is spilled to
    /// a slot, the walk reads them where they lie, and both are released
    /// afterwards — the two are owned temporaries the operator consumes, exactly
    /// as the equality path releases the values it compared. The surrounding
    /// integer comparison of the sign against zero recovers which ordering was
    /// written.
    ///
    /// [`equal_values`]: Self::equal_values
    /// [`CmpValue`]: kira_semantics_model::hir::HirBinaryOp::CmpValue
    pub(super) fn compare_values(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: a comparison is only lowered inside a function body, so the
        // builder is positioned in one and has a parent to allocate against.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let left_slot = self.entry_alloca(function, llvm_type, c"cmp.lhs");
        let right_slot = self.entry_alloca(function, llvm_type, c"cmp.rhs");
        // SAFETY: both slots address storage of `llvm_type`, both operands have
        // that type, and the builder is on a live block.
        unsafe {
            LLVMBuildStore(self.builder, left, left_slot);
            LLVMBuildStore(self.builder, right, right_slot);
        }
        let sign = self.compare_at(left_slot, right_slot, ty)?;
        self.release_at(left_slot, ty)?;
        self.release_at(right_slot, ty)?;
        Ok(sign)
    }

    /// Folds the value at `at` into `acc`, sending a struct through its hash leaf
    /// so a nested fold is a call rather than an inlined walk — the hash twin of
    /// [`compare_at`]. Nothing is consumed.
    ///
    /// [`compare_at`]: Self::compare_at
    pub(super) fn hash_at(
        &mut self,
        acc: LLVMValueRef,
        at: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !matches!(ty, Type::Struct(_)) {
            return self.hash_at_walk(acc, at, ty);
        }
        let leaf = self.leaf_callable(ty, Leaf::Hash)?;
        let mut args = [acc, at];
        Ok(self.call(leaf, &mut args, c"hash.struct"))
    }

    /// Structural `hash(v)` on one owned value of a statically known `Hashable`
    /// type, as the `i64` backing [`HashValue`].
    ///
    /// The counterpart of [`compare_values`]: the value is spilled to a slot, the
    /// walk folds it from the runtime seed, and the value is released afterwards —
    /// the operand is an owned temporary the operation consumes, exactly as the
    /// comparison paths release what they read.
    ///
    /// [`compare_values`]: Self::compare_values
    /// [`HashValue`]: kira_semantics_model::hir::HirBinaryOp
    pub(super) fn hash_values(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: a hash is only lowered inside a function body.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let slot = self.entry_alloca(function, llvm_type, c"hash.value");
        // SAFETY: `slot` addresses storage of `llvm_type`, `value` has that type,
        // and the builder is on a live block.
        unsafe { LLVMBuildStore(self.builder, value, slot) };
        let seed = self.call(self.runtime.hash_seed, &mut [], c"hash.seed");
        let acc = self.hash_at(seed, slot, ty)?;
        self.release_at(slot, ty)?;
        Ok(acc)
    }

    /// The slot a site spills `ty` into for its leaf, allocated once per
    /// function.
    fn scratch(&mut self, ty: Type, llvm_type: LLVMTypeRef) -> LLVMValueRef {
        // SAFETY: a value is only ever copied or dropped inside a function body
        // or a leaf, so the builder is positioned inside one.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let key = (function as usize, ty);
        if let Some(&existing) = self.scratch_slots.get(&key) {
            return existing;
        }
        let allocated = self.entry_alloca(function, llvm_type, c"glue.value");
        self.scratch_slots.insert(key, allocated);
        allocated
    }

    /// Marks a leaf as one an optimizing build folds back into its callers.
    ///
    /// Outlining is a compile-time decision, not a code-quality one: a release
    /// build runs the always-inliner (see `Module::emit_object`) and gets back
    /// exactly the machine code the expanded walk produced. A development build
    /// runs no inliner, so the attribute costs it nothing and the call stands.
    pub(super) fn mark_always_inline(&self, function: LLVMValueRef) {
        let attribute = c"alwaysinline";
        // SAFETY: `function` is a live function of this module, and the
        // attribute kind is a string LLVM copies.
        unsafe {
            let kind =
                LLVMGetEnumAttributeKindForName(attribute.as_ptr(), attribute.to_bytes().len());
            let value = LLVMCreateEnumAttribute(self.context, kind, 0);
            LLVMAddAttributeAtIndex(function, llvm_sys::LLVMAttributeFunctionIndex, value);
        }
    }

    /// Allocates a slot in `function`'s entry block, wherever the builder is.
    ///
    /// In the entry block rather than at the site, because a slot allocated
    /// inside a loop body grows the frame once per iteration — `alloca` is a
    /// stack bump, not a scope.
    pub(in crate::codegen) fn entry_alloca(
        &self,
        function: LLVMValueRef,
        llvm_type: LLVMTypeRef,
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: `function` is a live function of this module with an entry
        // block, and the builder is returned to the block it was building into.
        unsafe {
            let resume = LLVMGetInsertBlock(self.builder);
            let entry = LLVMGetEntryBasicBlock(function);
            let first = LLVMGetFirstInstruction(entry);
            if first.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, entry);
            } else {
                LLVMPositionBuilderBefore(self.builder, first);
            }
            let allocated = LLVMBuildAlloca(self.builder, llvm_type, name.as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, resume);
            allocated
        }
    }
}
