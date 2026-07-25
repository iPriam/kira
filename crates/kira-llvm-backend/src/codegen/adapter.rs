//! Generated foreign adapters: one uniform `extern "C"` wrapper per
//! `@FFI.Extern` import, and the marshalling a foreign call site does around it.
//!
//! # One ABI path for every backend
//!
//! A foreign call could lower a C signature differently in each engine. Instead
//! there is one proven path: the backend emits a single adapter per import with
//! the fixed shape
//!
//! ```text
//! extern "C" fn kira_foreign_adapter_<i>(
//!     args: *const BridgeValue, count: u32, out: *mut BridgeValue,
//! ) -> ForeignAdapterStatus
//! ```
//!
//! and both the native call site (here) and the VM sidecar host reach the real C
//! symbol only through it. The adapter validates `count` and each argument tag,
//! converts every payload to its exact C type (modulo narrowing on the way in,
//! sign/zero extension on the way out, `F32` rounding at the boundary, C `_Bool`,
//! target-width `RawPtr`, and transient `CString` storage), calls the C symbol,
//! and writes a checked result. A malformed input or an interior NUL returns a
//! status byte rather than corrupting memory.
//!
//! The same adapter definitions serve the executable (call sites here reference
//! them), the VM sidecar (a marker-only module exports them), and the hybrid
//! native half (its host resolves them by name).

use kira_runtime_abi::{ForeignAdapterStatus, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::{Callable, Codegen};
use crate::LlvmError;
use crate::adapter_name;

/// The LLVM attribute index of a function's return value.
const RETURN_INDEX: u32 = 0;

impl Codegen<'_> {
    /// Declares one adapter function per foreign import (no bodies yet).
    ///
    /// Every module that either calls or exports adapters declares them, so a
    /// call site can reference the adapter before its body exists.
    pub(super) fn declare_foreign_adapters(&mut self) -> Result<(), LlvmError> {
        for index in 0..self.program.foreign_imports.len() {
            let name = super::ffi::c_string(&adapter_name(index));
            // (args: ptr, count: i32, out: ptr) -> i32
            let mut params = [self.types.ptr, self.types.i32, self.types.ptr];
            // SAFETY: every type belongs to this module's context and `params`
            // outlives the type call.
            let callable = unsafe {
                let ty =
                    LLVMFunctionType(self.types.i32, params.as_mut_ptr(), params.len() as u32, 0);
                Callable {
                    ty,
                    value: LLVMAddFunction(self.module, name.as_ptr(), ty),
                }
            };
            self.foreign_adapters.push(callable);
        }
        Ok(())
    }

    /// Emits the body of every declared foreign adapter.
    pub(super) fn emit_foreign_adapters(&mut self) -> Result<(), LlvmError> {
        for index in 0..self.program.foreign_imports.len() {
            self.emit_foreign_adapter(index)?;
        }
        Ok(())
    }

    /// Emits the body of adapter `index`.
    fn emit_foreign_adapter(&mut self, index: usize) -> Result<(), LlvmError> {
        let import = self.program.foreign_imports[index].import.clone();
        let signature = import.signature();
        let params: Vec<ForeignType> = signature
            .parameters()
            .iter()
            .copied()
            .map(scalar_of)
            .collect::<Result<_, _>>()?;
        let result = scalar_of(signature.result())?;
        let adapter = self.foreign_adapters[index];
        let c_fn = self.declare_c_function(import.symbol(), &params, result);

        let types = self.types;
        let builder = self.builder;
        let context = self.context;

        // SAFETY: `adapter.value` is a freshly declared function in this live
        // module; every block, type, and value below belongs to its context, and
        // the builder is repositioned before each instruction it emits.
        unsafe {
            let entry = LLVMAppendBasicBlockInContext(context, adapter.value, c"entry".as_ptr());
            let bad_count =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"bad_count".as_ptr());
            let check_tags =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"check_tags".as_ptr());
            let bad_tag =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"bad_tag".as_ptr());
            let convert =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"convert".as_ptr());
            let interior_nul =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"interior_nul".as_ptr());
            let cleanup =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"cleanup".as_ptr());

            let args = LLVMGetParam(adapter.value, 0);
            let count = LLVMGetParam(adapter.value, 1);
            let out = LLVMGetParam(adapter.value, 2);

            // Entry: allocas for the status word and one slot per CString param.
            LLVMPositionBuilderAtEnd(builder, entry);
            let status = LLVMBuildAlloca(builder, types.i32, c"status".as_ptr());
            let mut cstring_slots = Vec::new();
            for (i, ft) in params.iter().enumerate() {
                if *ft == ForeignType::CString {
                    let slot = LLVMBuildAlloca(builder, types.ptr, c"cstr".as_ptr());
                    LLVMBuildStore(builder, LLVMConstPointerNull(types.ptr), slot);
                    cstring_slots.push((i, slot));
                }
            }
            // Reference the version marker so a stale sidecar fails to link by
            // name; the call is inert.
            self.call_runtime(self.runtime.foreign_marker, &mut [], c"");
            let expected_count = LLVMConstInt(types.i32, params.len() as u64, 0);
            let count_ok = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                count,
                expected_count,
                c"count.ok".as_ptr(),
            );
            LLVMBuildCondBr(builder, count_ok, check_tags, bad_count);

            LLVMPositionBuilderAtEnd(builder, bad_count);
            LLVMBuildRet(
                builder,
                LLVMConstInt(
                    types.i32,
                    u64::from(ForeignAdapterStatus::BAD_ARGUMENT_COUNT.0),
                    0,
                ),
            );

            // Tag validation: count is verified, so reading every tag is safe.
            LLVMPositionBuilderAtEnd(builder, check_tags);
            let mut all_ok = LLVMConstInt(types.i1, 1, 0);
            for (i, ft) in params.iter().enumerate() {
                let slot = self.bridge_element_ptr(args, i as u64);
                let tag = self.load_bridge_tag(slot);
                let expected = LLVMConstInt(types.i8, u64::from(ft.bridge_tag().0), 0);
                let eq = LLVMBuildICmp(
                    builder,
                    LLVMIntPredicate::LLVMIntEQ,
                    tag,
                    expected,
                    c"tag.ok".as_ptr(),
                );
                all_ok = LLVMBuildAnd(builder, all_ok, eq, c"tags.ok".as_ptr());
            }
            LLVMBuildCondBr(builder, all_ok, convert, bad_tag);

            LLVMPositionBuilderAtEnd(builder, bad_tag);
            LLVMBuildRet(
                builder,
                LLVMConstInt(
                    types.i32,
                    u64::from(ForeignAdapterStatus::BAD_ARGUMENT_TAG.0),
                    0,
                ),
            );

            // Convert every argument, calling the C symbol once all are ready.
            LLVMPositionBuilderAtEnd(builder, convert);
            let mut c_args = Vec::with_capacity(params.len());
            for (i, ft) in params.iter().enumerate() {
                let slot = self.bridge_element_ptr(args, i as u64);
                let payload = self.load_bridge_payload(slot);
                if *ft == ForeignType::CString {
                    let handle =
                        LLVMBuildIntToPtr(builder, payload, types.ptr, c"cstr.handle".as_ptr());
                    let c_ptr =
                        self.call_runtime(self.runtime.cstring_new, &mut [handle], c"cstr.ptr");
                    // Record before the null check so cleanup frees it on the
                    // success path, and the failing one stays null (no free).
                    let (_, cstr_slot) = cstring_slots
                        .iter()
                        .find(|(param, _)| *param == i)
                        .copied()
                        .ok_or(LlvmError::Unsupported(
                            "a CString slot the adapter never allocated",
                        ))?;
                    LLVMBuildStore(builder, c_ptr, cstr_slot);
                    let is_null = LLVMBuildICmp(
                        builder,
                        LLVMIntPredicate::LLVMIntEQ,
                        c_ptr,
                        LLVMConstPointerNull(types.ptr),
                        c"cstr.null".as_ptr(),
                    );
                    let ok =
                        LLVMAppendBasicBlockInContext(context, adapter.value, c"cstr.ok".as_ptr());
                    LLVMBuildCondBr(builder, is_null, interior_nul, ok);
                    LLVMPositionBuilderAtEnd(builder, ok);
                    c_args.push(c_ptr);
                } else {
                    c_args.push(self.foreign_arg_to_c(payload, *ft)?);
                }
            }
            let rc = LLVMBuildCall2(
                builder,
                c_fn.ty,
                c_fn.value,
                c_args.as_mut_ptr(),
                c_args.len() as u32,
                if result == ForeignType::Void {
                    c"".as_ptr()
                } else {
                    c"rc".as_ptr()
                },
            );
            self.store_foreign_result(out, rc, result)?;
            LLVMBuildStore(
                builder,
                LLVMConstInt(types.i32, u64::from(ForeignAdapterStatus::SUCCESS.0), 0),
                status,
            );
            LLVMBuildBr(builder, cleanup);

            // Interior NUL on some CString argument: report and clean up.
            LLVMPositionBuilderAtEnd(builder, interior_nul);
            LLVMBuildStore(
                builder,
                LLVMConstInt(
                    types.i32,
                    u64::from(ForeignAdapterStatus::INTERIOR_NUL.0),
                    0,
                ),
                status,
            );
            LLVMBuildBr(builder, cleanup);

            // Cleanup: free every transient C string (null-safe) and return the
            // recorded status.
            LLVMPositionBuilderAtEnd(builder, cleanup);
            for (_, slot) in &cstring_slots {
                let c_ptr = LLVMBuildLoad2(builder, types.ptr, *slot, c"cstr.free".as_ptr());
                self.call_runtime(self.runtime.cstring_free, &mut [c_ptr], c"");
            }
            let final_status = LLVMBuildLoad2(builder, types.i32, status, c"status.out".as_ptr());
            LLVMBuildRet(builder, final_status);
        }
        Ok(())
    }

    /// Declares the real C symbol with its exact-width signature and the
    /// sign/zero-extension attributes small integers need at the C ABI.
    fn declare_c_function(
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
    fn foreign_c_type(&self, ft: ForeignType) -> LLVMTypeRef {
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
    fn foreign_arg_to_c(
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
                ForeignType::RawPtr => {
                    LLVMBuildIntToPtr(builder, payload, types.ptr, c"a.ptr".as_ptr())
                }
                ForeignType::Void | ForeignType::CString => {
                    return Err(LlvmError::Unsupported(
                        "a foreign argument the adapter cannot marshal",
                    ));
                }
            }
        })
    }

    /// Writes the C call's result into `out`, tagged for the result foreign
    /// type, with the reserved bytes zeroed.
    fn store_foreign_result(
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
                ForeignType::CString => {
                    return Err(LlvmError::Unsupported("a foreign CString result"));
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
    fn load_bridge_tag(&self, slot: LLVMValueRef) -> LLVMValueRef {
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
                // `RawPtr` is already an `i64` word; both cross as-is.
                Type::Int(_) | Type::RawPtr => value,
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
                ForeignType::CString => {
                    return Err(LlvmError::Unsupported("a foreign CString result"));
                }
            }
        })
    }
}

/// The scalar a signature position names.
///
/// An aggregate position is refused rather than mapped: passing a C-layout
/// struct by value needs the platform's aggregate classification, which is not
/// something this backend derives. The frontend refuses an aggregate at the
/// seam, so this is a backstop on a signature that never reaches here.
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
