//! Lowering the expressions that build and read a runtime box.
//!
//! Everything about *what* a box is lives one level down, on `Codegen` — see
//! [`super::super::boxing`]. What is left here is the part that needs a function
//! body: evaluating the operands, and getting the drops in the right order
//! relative to the reads.

use kira_ir::IrExprId;
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Builds an enum value: a boxed tag plus its optional payload, encoded into
    /// the one type-erased word the runtime box carries.
    pub(super) fn lower_enum_new(
        &mut self,
        enum_id: kira_semantics_model::EnumId,
        tag: u32,
        payload: Option<IrExprId>,
    ) -> Result<LLVMValueRef, LlvmError> {
        let Some(payload) = payload else {
            // A variant with no payload is nothing but a tag, and the handle
            // holds it: `(tag << 1) | 1`, a constant, with no allocation and no
            // call. The runtime recognizes the low bit and treats clone as
            // identity and free as nothing. See `kira_native_bridge::enums`.
            return Ok(self.codegen.inline_enum(tag));
        };
        let payload_ty = self.codegen.enum_payload_type(enum_id, tag)?;
        let value = self.lower_expr(payload)?;
        let tag_value = self.codegen.const_int(i64::from(tag));
        self.codegen.box_new(tag_value, payload_ty, value, c"enum")
    }

    /// Boxes a value crossing into the top type.
    pub(super) fn lower_into_any(
        &mut self,
        value: IrExprId,
        from: Type,
        identity: kira_semantics_model::ErasedTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let lowered = self.lower_expr(value)?;
        self.codegen.erase_value(lowered, from, identity)
    }

    /// `value.type` where the type is known: the value is evaluated and
    /// released for its effects, and the answer is the id lowering interned.
    pub(super) fn lower_type_const(
        &mut self,
        value: IrExprId,
        id: kira_semantics_model::ErasedTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.type_of(value);
        let lowered = self.lower_expr(value)?;
        self.drop_value(lowered, ty)?;
        Ok(self.codegen.const_int(id.as_i64()))
    }

    /// A property of a runtime type descriptor, through the generated reader
    /// for that property.
    pub(super) fn lower_type_field(
        &mut self,
        descriptor: IrExprId,
        field: kira_semantics_model::TypeField,
    ) -> Result<LLVMValueRef, LlvmError> {
        let id = self.lower_expr(descriptor)?;
        let reader = self.codegen.type_field_reader(field)?;
        Ok(self.call(reader, &mut [id], c"type.field"))
    }

    /// `value.type` on an `Any`: the identity the box carries.
    pub(super) fn lower_type_of(&mut self, value: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let lowered = self.lower_expr(value)?;
        let tag = self.call(self.codegen.runtime.enum_tag, &mut [lowered], c"type.of");
        self.drop_value(lowered, Type::Any)?;
        Ok(tag)
    }

    /// Reads an enum value's discriminant tag as an `Int`.
    ///
    /// The VM's `EnumTag`, in the same order: the value is evaluated (a local
    /// read clones the enum), the tag is read out, and then the clone is freed —
    /// exactly as `.count` reads and frees an array.
    pub(super) fn lower_enum_tag(&mut self, value: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let enum_value = self.lower_expr(value)?;
        let tag = self.call(
            self.codegen.runtime.enum_tag,
            &mut [enum_value],
            c"enum.tag",
        );
        self.drop_value(enum_value, value_ty)?;
        Ok(tag)
    }

    /// Reads an enum value's payload as an owned value of type `ty`.
    ///
    /// The same order as the VM's `EnumPayload`: the enum is evaluated (a local
    /// read clones it), the payload is read *owned* — `kira_rt_enum_payload`
    /// clones a `String` — and only then is the enum released. Reading before
    /// releasing is what keeps a `String` payload alive across the free.
    pub(super) fn lower_enum_payload(
        &mut self,
        value: IrExprId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let enum_value = self.lower_expr(value)?;
        let decoded = self.codegen.read_box_payload(enum_value, ty)?;
        self.drop_value(enum_value, value_ty)?;
        Ok(decoded)
    }

    /// `value is Type`: the box's tag is the erased identity, so the test is
    /// one compare; the box is released either way.
    pub(super) fn lower_type_test(
        &mut self,
        value: IrExprId,
        target: kira_semantics_model::ErasedTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let boxed = self.lower_expr(value)?;
        let tag = self.call(self.codegen.runtime.enum_tag, &mut [boxed], c"any.tag");
        let expected = self.codegen.const_int(target.as_i64());
        // SAFETY: two `i64`s compared on a live block.
        let holds = unsafe {
            LLVMBuildICmp(
                self.codegen.builder,
                LLVMIntPredicate::LLVMIntEQ,
                tag,
                expected,
                c"any.is".as_ptr(),
            )
        };
        self.drop_value(boxed, value_ty)?;
        Ok(holds)
    }

    /// `value as Type`: the tag must be the erased identity, or the runtime
    /// traps; then the payload is read out as `ty` and the box released, as
    /// an enum payload is.
    pub(super) fn lower_type_cast(
        &mut self,
        value: IrExprId,
        target: kira_semantics_model::ErasedTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let boxed = self.lower_expr(value)?;
        let tag = self.call(self.codegen.runtime.enum_tag, &mut [boxed], c"any.tag");
        let expected = self.codegen.const_int(target.as_i64());
        // SAFETY: two `i64`s compared on a live block.
        let mismatch = unsafe {
            LLVMBuildICmp(
                self.codegen.builder,
                LLVMIntPredicate::LLVMIntNE,
                tag,
                expected,
                c"any.mismatch".as_ptr(),
            )
        };
        let mut args = [tag, expected];
        self.trap_if(mismatch, self.codegen.runtime.trap_cast, &mut args, c"cast.trap")?;
        let decoded = self.codegen.read_box_payload(boxed, ty)?;
        self.drop_value(boxed, value_ty)?;
        Ok(decoded)
    }
}
