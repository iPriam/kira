//! Deep copy and drop of a value, mirroring the VM's `Heap::copy_value` and
//! `Heap::drop_value`.
//!
//! These live on [`Codegen`] rather than on one function's lowering because a
//! value's shape is a program-wide fact, not a per-body one: the same walk that
//! copies a local read also fills the clone/free *leaf* an array's runtime
//! helpers call ([`super::elements`]), and a leaf is emitted with no function
//! body in scope. Everything here needs is the builder, the runtime
//! declarations, and the program's type table.

use kira_semantics_model::{StructId, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::ffi::c_string;
use super::types::Callable;

impl Codegen<'_> {
    /// Whether a value of `ty` owns heap storage that a copy must clone and a
    /// drop must release.
    pub(super) fn owns_heap(&self, ty: Type) -> bool {
        self.program.types.owns_heap(ty)
    }

    /// The element type of an array type.
    pub(super) fn element_of(&self, ty: Type) -> Result<Type, crate::LlvmError> {
        self.program
            .types
            .element_of(ty)
            .ok_or(crate::LlvmError::Unsupported("an element of a non-array"))
    }

    /// Produces an independent copy of `value`, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// Deep, field by field: a struct's copy clones every string it reaches, so
    /// no two live values share a handle and neither drop frees the other's.
    /// Scalars and structs of scalars copy for free — LLVM's `insertvalue`
    /// chain folds away — so the walk only costs anything where it must.
    pub(super) fn copy_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(value);
        }
        match ty {
            Type::String => Ok(self.call(self.runtime.str_clone, &mut [value], c"str.copy")),
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                let mut copy = value;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32);
                    let copied = self.copy_value(field, field_ty)?;
                    copy = self.insert_field(copy, copied, index as u32);
                }
                Ok(copy)
            }
            Type::Array(_) => {
                // The loop lives in the runtime, which is ordinary Rust there;
                // what the backend supplies is the element size and a leaf that
                // clones one element. See `kira-native-bridge`'s `array` module.
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let clone = self.element_clone(element)?;
                Ok(self.call(
                    self.runtime.array_clone,
                    &mut [value, esize, clone],
                    c"array.copy",
                ))
            }
            // An enum box clones through one generic helper: the box carries a
            // flag saying whether its payload is an owned string, so the backend
            // needs no per-variant leaf.
            Type::Enum(_) => Ok(self.call(self.runtime.enum_clone, &mut [value], c"enum.copy")),
            // `owns_heap` is only true for the cases above.
            _ => Err(crate::LlvmError::Unsupported("a copy of an unowned value")),
        }
    }

    /// Releases whatever heap storage `value` owns, mirroring the VM's
    /// `Heap::drop_value`.
    pub(super) fn drop_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::String => {
                self.call(self.runtime.str_free, &mut [value], c"");
                Ok(())
            }
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32);
                    self.drop_value(field, field_ty)?;
                }
                Ok(())
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let free = self.element_free(element)?;
                self.call(self.runtime.array_free, &mut [value, esize, free], c"");
                Ok(())
            }
            Type::Enum(_) => {
                self.call(self.runtime.enum_free, &mut [value], c"");
                Ok(())
            }
            _ => Err(crate::LlvmError::Unsupported("a drop of an unowned value")),
        }
    }

    /// The payload type of one enum variant, or an error when it has none.
    ///
    /// Only called for an [`kira_ir::IrExpr::EnumNew`] that carries a payload,
    /// so a payload-less variant here is a broken IR contract, not user input.
    pub(super) fn enum_payload_type(
        &self,
        id: kira_semantics_model::EnumId,
        tag: u32,
    ) -> Result<Type, crate::LlvmError> {
        self.program
            .types
            .enums()
            .get(id)
            .and_then(|def| def.variant(tag))
            .and_then(|variant| variant.payload)
            .ok_or(crate::LlvmError::Unsupported(
                "an enum payload the program never declared",
            ))
    }

    /// The field types of a declared struct.
    pub(super) fn field_types(&self, id: StructId) -> Result<Vec<Type>, crate::LlvmError> {
        self.program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.iter().map(|field| field.ty).collect())
            .ok_or(crate::LlvmError::Unsupported(
                "a struct the program never declared",
            ))
    }

    /// Reads field `index` out of a struct *value*.
    pub(super) fn extract_field(&self, value: LLVMValueRef, index: u32) -> LLVMValueRef {
        let name = c_string(&format!("field.{index}"));
        // SAFETY: `value` is a struct value with more than `index` fields — the
        // index came from that struct's own definition — and the builder is on
        // a live block.
        unsafe { LLVMBuildExtractValue(self.builder, value, index, name.as_ptr()) }
    }

    /// Returns `value` with field `index` replaced by `field`.
    pub(super) fn insert_field(
        &self,
        value: LLVMValueRef,
        field: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("with.{index}"));
        // SAFETY: as `extract_field`, and `field` has field `index`'s type.
        unsafe { LLVMBuildInsertValue(self.builder, value, field, index, name.as_ptr()) }
    }

    /// Emits a call to a runtime helper from within the current block.
    pub(super) fn call(
        &self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: the builder is on a live block and every call site supplies
        // arguments matching the callable's declared signature.
        unsafe { self.call_runtime(callable, args, name) }
    }
}
