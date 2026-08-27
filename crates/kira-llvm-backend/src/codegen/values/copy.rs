//! Copy and retained-storage walks for LLVM values.

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::Codegen;
use super::super::ffi::c_string;

impl Codegen<'_> {
    /// Whether a value of `ty` owns heap storage that a copy must clone and a
    /// drop must release.
    pub(in crate::codegen) fn owns_heap(&self, ty: Type) -> bool {
        self.program.types.owns_heap(ty)
    }

    /// Whether a copied value must replace unique C-block handles in the copy.
    pub(in crate::codegen) fn owns_unique_c_storage(&self, ty: Type) -> bool {
        self.program.types.owns_unique_c_storage(ty)
    }

    /// Whether `ty` can reach C storage a retained call must transfer.
    pub(in crate::codegen) fn contains_c_storage(&self, ty: Type) -> bool {
        self.program.types.contains_c_storage(ty)
    }

    /// The element type of an array type.
    pub(in crate::codegen) fn element_of(&self, ty: Type) -> Result<Type, crate::LlvmError> {
        self.program
            .types
            .element_of(ty)
            .ok_or(crate::LlvmError::internal("an element of a non-array"))
    }

    /// Takes a share of everything the value at `at` owns, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// # A copy is a retain
    ///
    /// Every arm below leaves the bits alone: a `String`, an array, an enum, an
    /// `Any` and a cell all copy by raising the share count of the object the
    /// handle already names, and a struct copies by doing that to each of its
    /// fields. So a copy never produces different bits from the ones it was
    /// given — the caller keeps the value it had, and this is only the counting.
    ///
    /// # By pointer, not by value
    ///
    /// A struct's field is reached with a `getelementptr` rather than by
    /// extracting it from the whole struct. A generated style struct is
    /// thousands of bytes, and at the code-generation level a development build
    /// uses, LLVM lowers a load of one into a move per field — so a walk that
    /// took the struct by value would spend more on loading it than on the
    /// counts it came to raise.
    ///
    /// The walk, not the site: this is emitted once per type, into that type's
    /// retain leaf. Fields go back through [`Codegen::retain_at`], so each
    /// field's own walk is a call to *its* leaf rather than more of this one.
    pub(in crate::codegen) fn retain_at_walk(
        &mut self,
        at: LLVMValueRef,
        ty: Type,
    ) -> Result<(), crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::CBlock => self.clone_cblock_at(at),
            Type::String => {
                let handle = self.load_handle(at, "str");
                self.copy_shared(handle, self.types.string_box, "str");
                Ok(())
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let def = self
                    .program
                    .types
                    .structs()
                    .get(id)
                    .ok_or(crate::LlvmError::internal(
                        "a struct the module never declared",
                    ))?
                    .clone();
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let field = self.field_pointer(struct_type, at, index as u32);
                    if def.owns_c_storage_at(index as u32) {
                        self.clone_cblock_at(field)?;
                        continue;
                    }
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    self.retain_at(field, field_ty)?;
                }
                Ok(())
            }
            // A copy takes a share of the array and walks nothing: the elements
            // are copied only if one of the two arrays is written, by the
            // runtime's mutable entry points, which is where the element clone
            // leaf goes instead. See `kira-native-bridge`'s `array` module.
            // Reading an array is most of what a frame does, and doing it
            // eagerly here was 78% of one.
            Type::Array(_) => {
                let handle = self.load_handle(at, "array");
                self.copy_shared(handle, self.types.array_header, "array");
                Ok(())
            }
            Type::Enum(_) => {
                let handle = self.load_handle(at, "enum");
                self.copy_shared(handle, self.types.enum_box, "enum");
                Ok(())
            }
            // An erased value copies exactly as an enum does, because its box
            // *is* an enum box: one more share of the same object. Nothing may
            // write through an `Any`, so two holders can observe nothing a deep
            // copy would have hidden — the same argument the enum arm rests on.
            Type::Any => {
                let handle = self.load_handle(at, "any");
                self.copy_shared(handle, self.types.enum_box, "any");
                Ok(())
            }
            // A cell copies as an enum does, because its box *is* an enum box —
            // and here the sharing is the point rather than an optimization
            // nobody can observe. A closure and the frame that declared the
            // `var` have to see each other's writes, so a copy must not be
            // independent. This is the one arm of this function that does not
            // preserve value semantics, because the type it copies does not
            // have them.
            Type::Cell(_) => {
                let handle = self.load_handle(at, "cell");
                self.copy_shared(handle, self.types.enum_box, "cell");
                Ok(())
            }
            // `owns_heap` is only true for the cases above.
            _ => Err(crate::LlvmError::internal("a copy of an unowned value")),
        }
    }

    /// Reads the handle a shared value is, out of the storage holding it.
    pub(in crate::codegen) fn load_handle(&self, at: LLVMValueRef, name: &str) -> LLVMValueRef {
        let name = c_string(&format!("{name}.handle"));
        // SAFETY: `at` addresses storage holding a handle, which is a `ptr`,
        // and the builder is on a live block.
        unsafe { LLVMBuildLoad2(self.builder, self.types.ptr, at, name.as_ptr()) }
    }

    /// Replaces the C-block handle at `at` with an independent deep clone.
    pub(in crate::codegen) fn clone_cblock_at(
        &mut self,
        at: LLVMValueRef,
    ) -> Result<(), crate::LlvmError> {
        // SAFETY: `at` addresses one live i64 C-block handle.
        let handle =
            unsafe { LLVMBuildLoad2(self.builder, self.types.i64, at, c"cblock".as_ptr()) };
        let clone = self.call(self.runtime.cblock_clone, &mut [handle], c"cblock.clone");
        // SAFETY: `at` is the destination slot and `clone` is its new handle.
        unsafe { LLVMBuildStore(self.builder, clone, at) };
        Ok(())
    }

    /// The address of field `index` inside the struct at `at`.
    pub(in crate::codegen) fn field_pointer(
        &self,
        struct_type: LLVMTypeRef,
        at: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("field.{index}.ptr"));
        // SAFETY: `at` addresses a value of `struct_type`, which has more than
        // `index` fields — the index came from that struct's own definition.
        unsafe { LLVMBuildStructGEP2(self.builder, struct_type, at, index, name.as_ptr()) }
    }
}
