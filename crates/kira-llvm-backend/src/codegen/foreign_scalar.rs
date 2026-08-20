//! The scalar half of the foreign seam: declaring the C callee, converting one
//! value between its Kira representation and its exact C type, and the
//! `BridgeValue` slots those travel in.
//!
//! Split out of [`super::adapter`], which emits the adapter *bodies*. These are
//! the pieces both that emission and the native call site reach for, and
//! keeping them together is what stops a conversion from being written twice
//! and drifting: the adapter narrows an argument on the way in and extends a
//! result on the way out, and an aggregate's member fields must be converted
//! exactly the same way or the VM and native backends disagree about the bytes
//! a C function received.

use kira_runtime_abi::ForeignType;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use crate::LlvmError;

impl Codegen<'_> {
    /// Declares a foreign symbol only to obtain its address for libffi.
    ///
    /// The declaration is deliberately `void ()`: LLVM never calls it, so no
    /// platform aggregate classification is encoded in the module. The
    /// bundled libffi runtime receives the address and the shared descriptor
    /// graph supplies the real C signature.
    /// Declares a DATA symbol and answers its address.
    ///
    /// Sibling of [`Self::declare_foreign_address`], which declares a function
    /// to take the address of. A linker resolves either by name, but an object
    /// declared as a function is a lie in the emitted IR -- and the one thing a
    /// reader of it must not conclude is that this symbol may be called.
    pub(super) fn declare_foreign_data(&self, symbol: &str) -> LLVMValueRef {
        let name = super::ffi::c_string(symbol);
        // SAFETY: the type is a placeholder for an object of unknown shape; only
        // the symbol's address is ever taken, never a load through this type.
        unsafe {
            let existing = LLVMGetNamedGlobal(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddGlobal(self.module, self.types.i8, name.as_ptr())
            } else {
                existing
            }
        }
    }

    pub(super) fn declare_foreign_address(&self, symbol: &str) -> LLVMValueRef {
        let name = super::ffi::c_string(symbol);
        // SAFETY: the function type belongs to this context and is used only
        // as an address declaration, never as a call signature.
        unsafe {
            let ty = LLVMFunctionType(self.types.void, std::ptr::null_mut(), 0, 0);
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                existing
            }
        }
    }

    /// Whether a foreign call on this target is a call to the symbol itself.
    ///
    /// A wasm module links its C in and has no loader to reach a second image
    /// with, so there is nothing for the bundled libffi path to call through.
    pub(super) fn calls_foreign_directly(&self) -> bool {
        matches!(self.target, super::plan::CodegenTarget::Wasm(_))
    }

    /// The scalar an aggregate crosses as when it has exactly one member.
    ///
    /// `None` for every other aggregate, which crosses behind a pointer.
    pub(super) fn single_scalar_member(
        &self,
        id: kira_runtime_abi::ForeignAggregateId,
    ) -> Result<Option<ForeignType>, LlvmError> {
        let aggregate = self
            .program
            .foreign_aggregates
            .get(id)
            .ok_or(LlvmError::internal("a call names an unknown aggregate"))?;
        Ok(match aggregate.members() {
            [kira_runtime_abi::ForeignMember::Scalar(ty)] => Some(*ty),
            _ => None,
        })
    }

    /// The LLVM type a foreign type crosses the C ABI as.
    pub(super) fn foreign_c_type(&self, ft: ForeignType) -> LLVMTypeRef {
        match ft {
            ForeignType::Void => self.types.void,
            ForeignType::I8 | ForeignType::U8 => self.types.i8,
            ForeignType::I16 | ForeignType::U16 => self.types.i16,
            ForeignType::I32 | ForeignType::U32 => self.types.i32,
            ForeignType::I64 | ForeignType::U64 => self.types.i64,
            ForeignType::Bool => self.types.i1,
            ForeignType::F32 => self.types.f32,
            ForeignType::F64 => self.types.f64,
            ForeignType::RawPtr | ForeignType::CString => self.types.ptr,
        }
    }

    /// Converts an `i64` bridge payload to the exact C value of a non-CString
    /// argument type.
    pub(super) fn foreign_arg_to_c(
        &self,
        payload: LLVMValueRef,
        ft: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: `payload` is an `i64` value on the live current block.
        Ok(unsafe {
            match ft {
                ForeignType::I8 | ForeignType::U8 => {
                    LLVMBuildTrunc(builder, payload, types.i8, c"a.i8".as_ptr())
                }
                ForeignType::I16 | ForeignType::U16 => {
                    LLVMBuildTrunc(builder, payload, types.i16, c"a.i16".as_ptr())
                }
                ForeignType::I32 | ForeignType::U32 => {
                    LLVMBuildTrunc(builder, payload, types.i32, c"a.i32".as_ptr())
                }
                ForeignType::I64 | ForeignType::U64 => payload,
                ForeignType::Bool => LLVMBuildTrunc(builder, payload, types.i1, c"a.bool".as_ptr()),
                ForeignType::F32 => {
                    let wide = LLVMBuildBitCast(builder, payload, types.f64, c"a.f64".as_ptr());
                    LLVMBuildFPTrunc(builder, wide, types.f32, c"a.f32".as_ptr())
                }
                ForeignType::F64 => {
                    LLVMBuildBitCast(builder, payload, types.f64, c"a.f64".as_ptr())
                }
                // A `CString` is a pointer word here. As a *parameter* it never
                // reaches this point — the adapter builds transient C storage
                // for it first — but as a member of a C-layout struct it is
                // already the address of storage that outlives the call, so it
                // crosses exactly as a `RawPtr` does.
                ForeignType::RawPtr | ForeignType::CString => {
                    LLVMBuildIntToPtr(builder, payload, types.ptr, c"a.ptr".as_ptr())
                }
                ForeignType::Void => {
                    return Err(LlvmError::internal(
                        "a foreign argument the adapter cannot marshal",
                    ));
                }
            }
        })
    }

    /// Pointer to element `index` of a `BridgeValue` array.
    pub(super) fn bridge_element_ptr(&self, base: LLVMValueRef, index: u64) -> LLVMValueRef {
        // SAFETY: `base` addresses at least `index + 1` bridge values on the live
        // current block.
        unsafe {
            let mut offset = [LLVMConstInt(self.types.i32, index, 0)];
            LLVMBuildInBoundsGEP2(
                self.builder,
                self.types.bridge_value,
                base,
                offset.as_mut_ptr(),
                1,
                c"bridge.elem".as_ptr(),
            )
        }
    }

    /// Writes a full `BridgeValue` at `slot`: tag, zeroed reserved bytes, and
    /// payload. The reserved bytes are zeroed because the adapter loader rejects
    /// a result whose reserved field is nonzero.
    pub(super) fn store_bridge(&self, slot: LLVMValueRef, tag: u8, payload: LLVMValueRef) {
        let types = self.types;
        // SAFETY: `slot` addresses a writable `BridgeValue` on the live block.
        unsafe {
            let tag_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                0,
                c"w.tag.ptr".as_ptr(),
            );
            LLVMBuildStore(
                self.builder,
                LLVMConstInt(types.i8, u64::from(tag), 0),
                tag_ptr,
            );
            let reserved_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                1,
                c"w.reserved.ptr".as_ptr(),
            );
            let reserved_ty = LLVMArrayType2(types.i8, 7);
            LLVMBuildStore(self.builder, LLVMConstNull(reserved_ty), reserved_ptr);
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"w.payload.ptr".as_ptr(),
            );
            LLVMBuildStore(self.builder, payload, payload_ptr);
        }
    }
}

impl Codegen<'_> {
    /// Reads a scalar result from the raw storage libffi filled.
    pub(super) fn read_raw_foreign_result(
        &self,
        storage: LLVMValueRef,
        result: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        if result == ForeignType::Void {
            // SAFETY: the type belongs to this live module context.
            return Ok(unsafe { LLVMConstInt(self.types.i1, 0, 0) });
        }
        let layout = kira_runtime_abi::scalar_layout(
            result,
            if self.pointer_width == kira_runtime_abi::ForeignPointerWidth::Bits32 {
                kira_runtime_abi::ForeignPointerWidth::Bits32
            } else {
                kira_runtime_abi::ForeignPointerWidth::Bits64
            },
        );
        let c_type = self.foreign_c_type(result);
        // SAFETY: `storage` is an alloca sized for this exact C scalar and the
        // load's alignment is the same shared ABI layout used by libffi.
        let loaded = unsafe {
            let value = LLVMBuildLoad2(self.builder, c_type, storage, c"ffi.result".as_ptr());
            LLVMSetAlignment(value, layout.align);
            value
        };
        if result == ForeignType::CString {
            // SAFETY: `loaded` is the pointer result of the C string storage
            // load, and the runtime declaration belongs to this module.
            return Ok(unsafe {
                self.call_runtime(self.runtime.str_from_cstr, &mut [loaded], c"ffi.string")
            });
        }
        self.c_value_to_kira(loaded, result)
    }
}

impl Codegen<'_> {
    /// Converts a Kira scalar value to the exact C type `ft` names.
    ///
    /// Goes through the same payload encoding and narrowing the adapter applies
    /// to an argument, so an aggregate's member field and a parameter of the
    /// same C type carry the identical bits.
    pub(super) fn kira_value_to_c(
        &self,
        value: LLVMValueRef,
        ft: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: `value` has the Kira representation of `ft` on the live block.
        let payload = unsafe {
            match ft {
                ForeignType::F32 | ForeignType::F64 => {
                    LLVMBuildBitCast(builder, value, types.i64, c"m.bits".as_ptr())
                }
                ForeignType::Bool => LLVMBuildZExt(builder, value, types.i64, c"m.wide".as_ptr()),
                // Every integer width and a `RawPtr` are already the VM's `i64`.
                _ => value,
            }
        };
        self.foreign_arg_to_c(payload, ft)
    }

    /// Converts a C-typed value of `ft` back to its Kira representation.
    ///
    /// The mirror of [`Codegen::kira_value_to_c`], extending by the declared
    /// signedness exactly as the adapter extends a scalar result.
    pub(super) fn c_value_to_kira(
        &self,
        value: LLVMValueRef,
        ft: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: `value` has `ft`'s C type on the live current block.
        Ok(unsafe {
            match ft {
                ForeignType::I8 | ForeignType::I16 | ForeignType::I32 => {
                    LLVMBuildSExt(builder, value, types.i64, c"m.sext".as_ptr())
                }
                ForeignType::U8 | ForeignType::U16 | ForeignType::U32 => {
                    LLVMBuildZExt(builder, value, types.i64, c"m.zext".as_ptr())
                }
                ForeignType::I64 | ForeignType::U64 => value,
                // C `_Bool` is one byte; Kira's `Bool` is an `i1`.
                ForeignType::Bool => LLVMBuildTrunc(builder, value, types.i1, c"m.bool".as_ptr()),
                ForeignType::F32 => LLVMBuildFPExt(builder, value, types.f64, c"m.f64".as_ptr()),
                ForeignType::F64 => value,
                // Read back as the opaque word it is: Kira never dereferences
                // a pointer it did not mint, so a `CString` member arrives as
                // the same pointer word a `RawPtr` member does.
                ForeignType::RawPtr | ForeignType::CString => {
                    LLVMBuildPtrToInt(builder, value, types.i64, c"m.ptr".as_ptr())
                }
                ForeignType::Void => {
                    return Err(LlvmError::internal(
                        "a `Void` member of a C-layout aggregate",
                    ));
                }
            }
        })
    }
}
