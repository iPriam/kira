//! Packing and unpacking `BridgeValue`s: how a value crosses between the two
//! engines of a hybrid program.
//!
//! Both directions of the boundary meet here. A trampoline *reads* the args the
//! host packed and *writes* the result back; a native-to-runtime call does the
//! mirror. One pair of routines serves both, so the two directions cannot
//! disagree about the layout.

use kira_runtime_abi::BridgeValueTag;
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use crate::LlvmError;

/// How a value of type `ty` sits in a `BridgeValue` payload.
enum PayloadForm {
    /// Already an `i64`.
    AsIs,
    /// A `double` reinterpreted as bits.
    FloatBits,
    /// A narrower integer widened.
    Widen,
    /// A pointer as an integer.
    PointerBits,
}

/// The bridge tag for `ty`, and how its payload is encoded.
fn bridge_tag_of(ty: Type) -> Result<(u8, Option<PayloadForm>), LlvmError> {
    Ok(match ty {
        Type::Void => (BridgeValueTag::VOID.0, None),
        Type::Int(_) => (BridgeValueTag::INT.0, Some(PayloadForm::AsIs)),
        Type::Float(_) => (BridgeValueTag::FLOAT.0, Some(PayloadForm::FloatBits)),
        Type::Bool => (BridgeValueTag::BOOL.0, Some(PayloadForm::Widen)),
        Type::String => (BridgeValueTag::STRING.0, Some(PayloadForm::PointerBits)),
        // A `BridgeValue` is 16 bytes with a one-word payload; a struct does
        // not fit and has no tag. Crossing the seam with one needs an ABI
        // decision (by value? by pointer? who frees the strings inside?) that
        // has not been made, so the boundary says no rather than guessing.
        Type::Struct(_) => return Err(LlvmError::StructAtSeam),
        // An array does not fit either, but the reason is different: the
        // language does let one cross, and what is missing is the ownership
        // answer at the boundary — who frees the elements, and what a native
        // callee growing the array means for the other half. See
        // `BridgeValueTag::ARRAY`.
        Type::Array(_) => return Err(LlvmError::ArrayAtSeam),
        // An enum does not fit either, and on the same grounds as a struct: it
        // is a tagged value with no one-word form, and how it would cross is
        // undecided. See `BridgeValueTag::ENUM`.
        Type::Enum(_) => return Err(LlvmError::EnumAtSeam),
        Type::Error => return Err(LlvmError::Unsupported("a value with no type")),
    })
}

impl Codegen<'_> {
    /// Reads one `BridgeValue`'s payload as a value of type `ty`.
    ///
    /// The tag is not consulted: the static type is what the manifest promised
    /// and what the other side encoded from. The tag exists so a *reader* that
    /// does not know the signature can still refuse an unknown value.
    pub(super) fn read_bridge_payload(
        &self,
        slot: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        // SAFETY: `slot` points at a `BridgeValue` the caller supplied, and the
        // builder is on a live block.
        unsafe {
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"arg.payload.ptr".as_ptr(),
            );
            let payload = LLVMBuildLoad2(
                self.builder,
                types.i64,
                payload_ptr,
                c"arg.payload".as_ptr(),
            );
            Ok(match ty {
                Type::Int(_) => payload,
                Type::Float(_) => {
                    LLVMBuildBitCast(self.builder, payload, types.f64, c"arg.float".as_ptr())
                }
                Type::Bool => LLVMBuildTrunc(self.builder, payload, types.i1, c"arg.bool".as_ptr()),
                Type::String => {
                    LLVMBuildIntToPtr(self.builder, payload, types.ptr, c"arg.str".as_ptr())
                }
                Type::Struct(_) => return Err(LlvmError::StructAtSeam),
                Type::Array(_) => return Err(LlvmError::ArrayAtSeam),
                Type::Enum(_) => return Err(LlvmError::EnumAtSeam),
                Type::Void | Type::Error => {
                    return Err(LlvmError::Unsupported("a parameter with no runtime value"));
                }
            })
        }
    }

    /// Writes `value` into the `BridgeValue` at `slot`, tagged for `ty`.
    pub(super) fn write_bridge_value(
        &self,
        slot: LLVMValueRef,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let (tag, payload) = bridge_tag_of(ty)?;
        // SAFETY: `slot` points at a writable `BridgeValue`, and the builder is
        // on a live block.
        unsafe {
            let payload = match payload {
                // Void carries no payload; zero keeps the reserved word defined.
                None => LLVMConstInt(types.i64, 0, 0),
                Some(PayloadForm::AsIs) => value,
                Some(PayloadForm::FloatBits) => {
                    LLVMBuildBitCast(self.builder, value, types.i64, c"ret.bits".as_ptr())
                }
                Some(PayloadForm::Widen) => {
                    LLVMBuildZExt(self.builder, value, types.i64, c"ret.wide".as_ptr())
                }
                Some(PayloadForm::PointerBits) => {
                    LLVMBuildPtrToInt(self.builder, value, types.i64, c"ret.handle".as_ptr())
                }
            };
            let tag_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                0,
                c"ret.tag.ptr".as_ptr(),
            );
            LLVMBuildStore(
                self.builder,
                LLVMConstInt(types.i8, u64::from(tag), 0),
                tag_ptr,
            );
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"ret.payload.ptr".as_ptr(),
            );
            LLVMBuildStore(self.builder, payload, payload_ptr);
        }
        Ok(())
    }
}
