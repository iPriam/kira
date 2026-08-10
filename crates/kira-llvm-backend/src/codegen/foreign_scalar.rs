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

use kira_runtime_abi::{ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::{Callable, Codegen};
use crate::LlvmError;

/// The LLVM attribute index of a function's return value.
const RETURN_INDEX: u32 = 0;

impl Codegen<'_> {
    pub(super) fn declare_shim_function(
        &self,
        index: usize,
        params: &[ForeignTypeSpec],
        result: ForeignTypeSpec,
    ) -> Callable {
        let name = super::ffi::c_string(&crate::shim::shim_name(index));
        let mut param_types: Vec<LLVMTypeRef> = Vec::with_capacity(params.len() + 1);
        if result.aggregate().is_some() {
            param_types.push(self.types.ptr);
        }
        for spec in params {
            param_types.push(match spec {
                ForeignTypeSpec::Aggregate(_) => self.types.ptr,
                ForeignTypeSpec::Scalar(ty) => self.foreign_c_type(*ty),
            });
        }
        let return_type = match result {
            ForeignTypeSpec::Aggregate(_) => self.foreign_c_type(ForeignType::Void),
            ForeignTypeSpec::Scalar(ty) => self.foreign_c_type(ty),
        };
        // SAFETY: every type belongs to this module's context, and `param_types`
        // outlives the calls below.
        unsafe {
            let ty = LLVMFunctionType(
                return_type,
                param_types.as_mut_ptr(),
                param_types.len() as u32,
                0,
            );
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let value = if existing.is_null() {
                LLVMAddFunction(self.module, name.as_ptr(), ty)
            } else {
                existing
            };
            Callable { ty, value }
        }
    }

    /// Declares the real C symbol with its exact-width signature and the
    /// sign/zero-extension attributes small integers need at the C ABI.
    pub(super) fn declare_c_function(
        &self,
        symbol: &str,
        params: &[ForeignType],
        result: ForeignType,
    ) -> Callable {
        let name = super::ffi::c_string(symbol);
        let mut param_types: Vec<LLVMTypeRef> =
            params.iter().map(|ft| self.foreign_c_type(*ft)).collect();
        let return_type = self.foreign_c_type(result);
        // SAFETY: every type belongs to this module's context; `param_types`
        // outlives the calls below.
        unsafe {
            let ty = LLVMFunctionType(
                return_type,
                param_types.as_mut_ptr(),
                param_types.len() as u32,
                0,
            );
            let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
            let value = if existing.is_null() {
                let function = LLVMAddFunction(self.module, name.as_ptr(), ty);
                if let Some(attr) = ext_attr(result) {
                    self.add_ext_attr(function, RETURN_INDEX, attr);
                }
                for (i, ft) in params.iter().enumerate() {
                    if let Some(attr) = ext_attr(*ft) {
                        self.add_ext_attr(function, (i + 1) as u32, attr);
                    }
                }
                function
            } else {
                existing
            };
            Callable { ty, value }
        }
    }

    /// Adds an enum extension attribute (`signext`/`zeroext`) at `index`.
    fn add_ext_attr(&self, function: LLVMValueRef, index: u32, name: &str) {
        // SAFETY: `name` is a known enum attribute spelling, `function` is a live
        // function in this context, and `index` is a valid attribute index.
        unsafe {
            let kind = LLVMGetEnumAttributeKindForName(name.as_ptr().cast(), name.len());
            let attr = LLVMCreateEnumAttribute(self.context, kind, 0);
            LLVMAddAttributeAtIndex(function, index, attr);
        }
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
                    return Err(LlvmError::Unsupported(
                        "a foreign argument the adapter cannot marshal",
                    ));
                }
            }
        })
    }

    /// Writes the C call's result into `out`, tagged for the result foreign
    /// type, with the reserved bytes zeroed.
    pub(super) fn store_foreign_result(
        &self,
        out: LLVMValueRef,
        rc: LLVMValueRef,
        result: ForeignType,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: `rc` has the C result type on the live current block, and `out`
        // addresses one writable `BridgeValue`.
        let payload = unsafe {
            match result {
                ForeignType::Void => LLVMConstInt(types.i64, 0, 0),
                ForeignType::I8 | ForeignType::I16 | ForeignType::I32 => {
                    LLVMBuildSExt(builder, rc, types.i64, c"r.sext".as_ptr())
                }
                ForeignType::U8 | ForeignType::U16 | ForeignType::U32 => {
                    LLVMBuildZExt(builder, rc, types.i64, c"r.zext".as_ptr())
                }
                ForeignType::I64 | ForeignType::U64 => rc,
                ForeignType::Bool => LLVMBuildZExt(builder, rc, types.i64, c"r.bool".as_ptr()),
                ForeignType::F32 => {
                    let wide = LLVMBuildFPExt(builder, rc, types.f64, c"r.f64".as_ptr());
                    LLVMBuildBitCast(builder, wide, types.i64, c"r.bits".as_ptr())
                }
                ForeignType::F64 => LLVMBuildBitCast(builder, rc, types.i64, c"r.bits".as_ptr()),
                ForeignType::RawPtr => {
                    LLVMBuildPtrToInt(builder, rc, types.i64, c"r.word".as_ptr())
                }
                // A returned C string leaves the adapter as a **string handle**,
                // exactly as an argument enters it as one: the adapter owns the
                // C-side conversion in both directions, so no engine ever holds
                // a `const char*`.
                //
                // Copying here rather than at the call site is what makes it
                // correct, not just tidy. The cleanup block below frees this
                // call's transient argument copies, and a C function may return
                // a pointer *into* one of them — `strchr` does, and so does any
                // "return the input unchanged" path. Reading it afterwards is a
                // use-after-free nothing downstream could detect, so the bytes
                // are taken while the pointer is still good.
                ForeignType::CString => {
                    let handle =
                        self.call_runtime(self.runtime.str_from_cstr, &mut [rc], c"r.cstr.str");
                    LLVMBuildPtrToInt(builder, handle, types.i64, c"r.handle".as_ptr())
                }
            }
        };
        self.store_bridge(out, result.bridge_tag().0, payload);
        Ok(())
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

    /// Loads the tag byte of a `BridgeValue` at `slot`.
    pub(super) fn load_bridge_tag(&self, slot: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: `slot` addresses a `BridgeValue` on the live current block.
        unsafe {
            let ptr = LLVMBuildStructGEP2(
                self.builder,
                self.types.bridge_value,
                slot,
                0,
                c"tag.ptr".as_ptr(),
            );
            LLVMBuildLoad2(self.builder, self.types.i8, ptr, c"tag".as_ptr())
        }
    }

    /// Loads the `i64` payload of a `BridgeValue` at `slot`.
    pub(super) fn load_bridge_payload(&self, slot: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: `slot` addresses a `BridgeValue` on the live current block.
        unsafe {
            let ptr = LLVMBuildStructGEP2(
                self.builder,
                self.types.bridge_value,
                slot,
                2,
                c"payload.ptr".as_ptr(),
            );
            LLVMBuildLoad2(self.builder, self.types.i64, ptr, c"payload".as_ptr())
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
    /// Writes one foreign-call argument into a `BridgeValue`, tagged for the
    /// foreign parameter type and encoded from the Kira argument value.
    pub(super) fn write_foreign_arg(
        &self,
        slot: LLVMValueRef,
        value: LLVMValueRef,
        kira_ty: Type,
        foreign_ty: ForeignType,
    ) -> Result<(), LlvmError> {
        let types = self.types;
        let builder = self.builder;
        // SAFETY: `value` has `kira_ty`'s LLVM type on the live block.
        let payload = unsafe {
            match kira_ty {
                // Every integer width shares the VM's `i64` representation and a
                // pointer word is already an `i64` word; both cross as-is. A
                // typed `@FFI.Pointer` is that same word — the two widen into
                // each other, so a value one symbol handed back as `T *` is
                // passed to the next by the name that symbol gave it.
                Type::Int(_) | Type::RawPtr | Type::ForeignPtr(_) => value,
                Type::Float(_) => LLVMBuildBitCast(builder, value, types.i64, c"arg.bits".as_ptr()),
                Type::Bool => LLVMBuildZExt(builder, value, types.i64, c"arg.wide".as_ptr()),
                // A `String` argument crosses to a `CString` parameter as its
                // opaque heap handle; the adapter copies it to transient C
                // storage. `write_foreign_arg` never sees a `CString`-typed Kira
                // value, only the `String` the caller passes.
                Type::String => {
                    LLVMBuildPtrToInt(builder, value, types.i64, c"arg.handle".as_ptr())
                }
                _ => {
                    return Err(LlvmError::Unsupported(
                        "a foreign argument whose Kira type cannot cross the seam",
                    ));
                }
            }
        };
        self.store_bridge(slot, foreign_ty.bridge_tag().0, payload);
        Ok(())
    }

    /// Reads a foreign adapter's result back as the Kira value of the call.
    pub(super) fn read_foreign_result(
        &self,
        out: LLVMValueRef,
        result: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        let types = self.types;
        let builder = self.builder;
        let payload = self.load_bridge_payload(out);
        // SAFETY: `payload` is the `i64` result payload on the live block.
        Ok(unsafe {
            match result {
                // Every integer width and a `RawPtr` are an `i64` Kira value; the
                // adapter already extended the C result into the payload.
                //
                // NOTE: the Rust seams (`foreign::check_pointer_width`,
                // `kira-dynamic-ffi`, `kira-hybrid-runtime`) reject a `RawPtr`
                // word with bits set above the target pointer width; this native
                // read does not. A no-op on 64-bit hosts (the only supported
                // targets today, where the payload width equals the pointer
                // width). When a 32-bit target is added, mirror that width check
                // here so both seams encode the identical contract.
                ForeignType::I8
                | ForeignType::I16
                | ForeignType::I32
                | ForeignType::I64
                | ForeignType::U8
                | ForeignType::U16
                | ForeignType::U32
                | ForeignType::U64
                | ForeignType::RawPtr => payload,
                ForeignType::Bool => LLVMBuildTrunc(builder, payload, types.i1, c"r.bool".as_ptr()),
                ForeignType::F32 | ForeignType::F64 => {
                    LLVMBuildBitCast(builder, payload, types.f64, c"r.float".as_ptr())
                }
                // A `Void` foreign call yields no value; the caller `Eval`s it and
                // never names the placeholder.
                ForeignType::Void => LLVMConstInt(types.i1, 0, 0),
                // The adapter already copied the callee's bytes into a string
                // handle, so this is the ordinary owned Kira `String` and there
                // is nothing here to free — the same value the VM's seam and the
                // hybrid host lift out of the same word.
                ForeignType::CString => {
                    LLVMBuildIntToPtr(builder, payload, types.ptr, c"r.str".as_ptr())
                }
            }
        })
    }
}

impl Codegen<'_> {
    /// Writes a pointer into a `BridgeValue` slot under `tag`.
    ///
    /// The aggregate contract in one place: the payload word is the address of
    /// C-layout bytes the *caller* owns for the length of the call, so nothing
    /// crosses ownership in either direction.
    pub(super) fn write_bridge_pointer(
        &self,
        slot: LLVMValueRef,
        pointer: LLVMValueRef,
        tag: kira_runtime_abi::BridgeValueTag,
    ) {
        // SAFETY: `pointer` is a live pointer value on the current block and
        // `slot` addresses one writable bridge value.
        let payload = unsafe {
            LLVMBuildPtrToInt(self.builder, pointer, self.types.i64, c"agg.word".as_ptr())
        };
        self.store_bridge(slot, tag.0, payload);
    }

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
                    return Err(LlvmError::Unsupported(
                        "a `Void` member of a C-layout aggregate",
                    ));
                }
            }
        })
    }
}

/// The scalar a signature position names.
///
/// An aggregate position is refused rather than mapped: passing a C-layout
/// struct by value needs the platform's aggregate classification, which is not
/// something this backend derives. A signature carrying one is routed through
/// the generated C shim instead, so reaching this is a backstop.
pub(super) fn scalar_of(spec: ForeignTypeSpec) -> Result<ForeignType, LlvmError> {
    spec.scalar().ok_or(LlvmError::Unsupported(
        "a C-layout aggregate at the foreign seam",
    ))
}

/// The sign/zero-extension attribute a small integer C type needs, if any.
fn ext_attr(ft: ForeignType) -> Option<&'static str> {
    match ft {
        ForeignType::I8 | ForeignType::I16 => Some("signext"),
        ForeignType::U8 | ForeignType::U16 | ForeignType::Bool => Some("zeroext"),
        _ => None,
    }
}
