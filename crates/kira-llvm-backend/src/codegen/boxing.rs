//! The runtime box: how an enum value and an erased `Any` are built and read.
//!
//! One module because they are one box. A Kira enum on the native side is a tag,
//! a payload kind saying what the payload word owns, and that word; an `Any` is
//! those same three fields with the tag naming the *type* that was erased rather
//! than a variant. They share their construction, their payload encoding, and —
//! through `kira_rt_enum_clone`/`kira_rt_enum_free` — everything that copies and
//! releases them, so splitting them apart would leave two places to disagree
//! about what a payload word means.
//!
//! These sit on [`Codegen`] rather than on one function's lowering because a box
//! is built from generated glue as well as from an expression
//! ([`super::lower::FunctionLowering`]), and glue has no Kira function in
//! scope at all.

use kira_runtime_abi::EnumPayloadKind;
use kira_semantics_model::{ErasedTypeId, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use crate::LlvmError;

impl Codegen<'_> {
    /// Builds a box holding `tag` and `value`, a payload of type `payload_ty`.
    ///
    /// The one place a box is made, whether the tag names a variant or an erased
    /// type: a scalar payload's bits go in directly, a `String` or nested handle
    /// goes in owned, and a struct or array goes through the runtime's erased
    /// aggregate box. Ownership of `value` passes to the box, which is what makes the
    /// box's clone/free reclaim it.
    pub(in crate::codegen) fn box_new(
        &mut self,
        tag: LLVMValueRef,
        payload_ty: Type,
        value: LLVMValueRef,
        name: &std::ffi::CStr,
    ) -> Result<LLVMValueRef, LlvmError> {
        if matches!(payload_ty, Type::Struct(_) | Type::Array(_)) {
            return self.aggregate_box_new(tag, payload_ty, value);
        }
        let (kind, payload_word) = self.encode_box_payload(payload_ty, value)?;
        Ok(self.call(self.runtime.enum_new, &mut [tag, kind, payload_word], name))
    }

    /// Boxes a value crossing into the top type.
    ///
    /// Native code has nowhere to keep a value's type once the static type stops
    /// saying it, so this allocates the box that does: a tag naming the type
    /// that crossed in, the payload kind the runtime's clone and free read, and
    /// the value's one-word form. The box is the enum box, reused rather than
    /// reinvented — which is what makes an erased value copy and drop through
    /// `kira_rt_enum_clone`/`kira_rt_enum_free` with no runtime helper added
    /// for carrying `Any` at all.
    ///
    /// # Why the tag is a type and not a kind
    ///
    /// It was `ErasedKind` — eight coarse families, of which `STRUCT` is one
    /// — and nothing read it. Comparing two erased values does, and a kind is
    /// not enough: an aggregate is untyped bytes plus generated leaves here, so
    /// reading a `Rect`'s through a `Point`'s leaf is undefined behavior rather
    /// than a wrong answer. An `ErasedTypeId` names the type exactly, and it
    /// encodes its own family in the high word, so the runtime can still tell a
    /// float from an int when it has to compare payload words as floats.
    ///
    /// The VM writes the same id; see `kira-bytecode`'s expression compiler and
    /// `IrExpr::IntoAny` for why one design has two mechanics.
    pub(in crate::codegen) fn erase_value(
        &mut self,
        value: LLVMValueRef,
        from: Type,
        identity: ErasedTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let tag = self.const_int(identity.as_i64());
        // A struct is wider than one word, and an array's clone and free are
        // type-specific, so both take the runtime's erased aggregate payload —
        // the same one an aggregate enum payload already uses. Everything else fits
        // the word directly.
        self.box_new(tag, from, value, c"any")
    }

    /// Moves an aggregate payload into the runtime's erased aggregate box.
    fn aggregate_box_new(
        &mut self,
        tag: LLVMValueRef,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        let size = self.abi_size(ty)?;
        // SAFETY: `llvm_type` belongs to this context, `value` has that type, and
        // the builder is positioned on a live block.
        let (slot, saved) = self.dynamic_alloca(llvm_type, c"enum.aggregate.source");
        self.lifetime_start(slot);
        // SAFETY: `slot` was allocated with `llvm_type` and `value` has that
        // same type.
        unsafe { LLVMBuildStore(self.builder, value, slot) };
        let clone = self.element_clone(ty)?;
        let free = self.element_free(ty)?;
        // The equality leaf travels with the other two so an erased aggregate
        // can be compared. It goes through the constructor appended beside the
        // original rather than through a changed signature, which is what keeps
        // this additive at the ABI.
        let eq = self.element_eq(ty)?;
        let result = self.call(
            self.runtime.enum_new_aggregate_eq,
            &mut [tag, slot, size, clone, free, eq],
            c"enum.aggregate",
        );
        self.release_dynamic_alloca(slot, saved);
        Ok(result)
    }

    /// Reads a box's payload as an owned value of type `ty`.
    ///
    /// The runtime hands back a value the caller owns — a `String` payload is
    /// cloned — and leaves the box exactly as it found it, so the box is still
    /// the caller's to release afterwards.
    pub(in crate::codegen) fn read_box_payload(
        &mut self,
        boxed: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !matches!(ty, Type::Struct(_) | Type::Array(_)) {
            let word = self.call(self.runtime.enum_payload, &mut [boxed], c"enum.payload");
            return self.decode_box_payload(ty, word);
        }
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this context and the runtime writes
        // one owned value of exactly that type into `out`.
        let (out, saved) = self.dynamic_alloca(llvm_type, c"enum.aggregate.payload");
        self.lifetime_start(out);
        self.call(self.runtime.enum_payload_aggregate, &mut [boxed, out], c"");
        // SAFETY: the helper initialized `out` with a value of `llvm_type`.
        let value = unsafe {
            LLVMBuildLoad2(
                self.builder,
                llvm_type,
                out,
                c"enum.aggregate.value".as_ptr(),
            )
        };
        self.release_dynamic_alloca(out, saved);
        Ok(value)
    }

    /// Encodes a payload value into `(payload_kind, payload_word)` for the box.
    pub(in crate::codegen) fn encode_box_payload(
        &self,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(LLVMValueRef, LLVMValueRef), LlvmError> {
        let builder = self.builder;
        let types = self.types;
        // SAFETY: `value` has `ty`'s LLVM type and the builder is on a live
        // block; each conversion below targets `i64`, the box's payload word.
        let word = unsafe {
            match ty {
                Type::Int(_) => value,
                Type::Float(_) => {
                    LLVMBuildBitCast(builder, value, types.i64, c"enum.float.bits".as_ptr())
                }
                Type::Bool => LLVMBuildZExt(builder, value, types.i64, c"enum.bool.bits".as_ptr()),
                // A `RawPtr` is already the box's word: an opaque `i64` nothing
                // dereferences or frees. It reaches here only through an `Any`
                // — no enum variant declares a `RawPtr` payload — and it needs
                // no conversion when it does.
                // A task handle is a word naming a row in the running
                // program's task table, so it is already the box's word too.
                Type::RawPtr | Type::ForeignPtr(_) | Type::Task(_) => value,
                // A nested enum is a handle exactly as a `String` is, so it
                // encodes the same way; only the kind the box records differs,
                // which is what makes its clone/free recurse.
                // An erased value is a handle to an enum-shaped box, so it
                // encodes exactly as a nested enum does.
                Type::String | Type::Enum(_) | Type::Any => {
                    LLVMBuildPtrToInt(builder, value, types.i64, c"enum.handle.bits".as_ptr())
                }
                _ => {
                    return Err(LlvmError::internal(
                        "an enum payload of an unsupported type",
                    ));
                }
            }
        };
        let kind = self.const_int(payload_kind(ty));
        Ok((kind, word))
    }

    /// Decodes a payload word back into a value of type `ty`.
    ///
    /// The exact inverse of [`Codegen::encode_box_payload`], which is what makes
    /// a round trip through the box lossless on every payload type the
    /// declaration admits.
    pub(in crate::codegen) fn decode_box_payload(
        &self,
        ty: Type,
        word: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.builder;
        let types = self.types;
        // SAFETY: `word` is the `i64` the box stores for a payload of `ty`, put
        // there by `encode_box_payload`, and the builder is on a live block.
        unsafe {
            Ok(match ty {
                Type::Int(_) | Type::RawPtr | Type::ForeignPtr(_) | Type::Task(_) => word,
                Type::Float(_) => {
                    LLVMBuildBitCast(builder, word, types.f64, c"enum.payload.float".as_ptr())
                }
                Type::Bool => {
                    LLVMBuildTrunc(builder, word, types.i1, c"enum.payload.bool".as_ptr())
                }
                Type::String | Type::Enum(_) | Type::Any => {
                    LLVMBuildIntToPtr(builder, word, types.ptr, c"enum.payload.handle".as_ptr())
                }
                _ => {
                    return Err(LlvmError::internal(
                        "an enum payload of an unsupported type",
                    ));
                }
            })
        }
    }
}

/// The payload kind the enum box records for a payload of type `ty`.
///
/// Mirrors `kira_native_bridge::enums`' `PAYLOAD_*` constants, which decide what
/// the box's clone and free reclaim. The two are kept in step by
/// `the_payload_kinds_match_the_runtime`, below — the backend and the runtime
/// archive are compiled separately, so nothing but a test makes them agree.
fn payload_kind(ty: Type) -> i64 {
    match ty {
        Type::String => EnumPayloadKind::STR,
        // An erased value is an enum box, so the payload kind that reclaims a
        // nested enum reclaims one of these too.
        Type::Enum(_) | Type::Any => EnumPayloadKind::ENUM,
        Type::Struct(_) | Type::Array(_) => EnumPayloadKind::AGGREGATE,
        _ => EnumPayloadKind::INERT,
    }
    .as_i64()
}

#[cfg(test)]
mod tests {
    use super::payload_kind;
    use kira_runtime_abi::EnumPayloadKind;
    use kira_semantics_model::{EnumDef, EnumTable, StructDef, Type, TypeTable};

    /// The kinds this lowering emits are the ones the runtime interprets.
    ///
    /// A drift here is the silent failure the ABI marker exists to catch: the
    /// symbols still resolve, and the box simply forgets to free its payload.
    #[test]
    fn the_payload_kinds_match_the_runtime() {
        assert_eq!(payload_kind(Type::INT), EnumPayloadKind::INERT.as_i64());
        assert_eq!(payload_kind(Type::Bool), EnumPayloadKind::INERT.as_i64());
        assert_eq!(payload_kind(Type::String), EnumPayloadKind::STR.as_i64());
        // An id is minted only by the table, so the test declares one.
        let mut enums = EnumTable::new();
        let id = enums
            .declare(EnumDef {
                name: "E".to_owned(),
                variants: Vec::new(),
            })
            .expect("a fresh table accepts the first declaration");
        assert_eq!(payload_kind(Type::Enum(id)), EnumPayloadKind::ENUM.as_i64());

        let mut types = TypeTable::new();
        let id = types
            .structs_mut()
            .declare(StructDef {
                name: "Payload".to_owned(),
                fields: Vec::new(),
                c_layout: false,
                drop_glue: None,
            })
            .expect("a fresh table accepts the first struct");
        assert_eq!(
            payload_kind(Type::Struct(id)),
            EnumPayloadKind::AGGREGATE.as_i64()
        );
        // An erased array travels the same way a struct does: through the
        // aggregate box, with generated clone/free leaves.
        let array = types.array_of(Type::INT);
        assert_eq!(payload_kind(array), EnumPayloadKind::AGGREGATE.as_i64());
    }
}
