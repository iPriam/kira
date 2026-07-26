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
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;

use super::foreign_scalar::scalar_of;
use super::{Callable, Codegen};
use crate::LlvmError;
use crate::adapter_name;

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
        let specs: Vec<ForeignTypeSpec> = signature.parameters().to_vec();
        let result_spec = signature.result();
        // An aggregate anywhere in the signature routes the call through the
        // generated C shim, which takes every aggregate by pointer. The adapter
        // then never has a by-value struct in its own IR — the C compiler that
        // built the shim owns that ABI decision entirely.
        let via_shim = signature.has_aggregate();
        let adapter = self.foreign_adapters[index];
        let callee = if via_shim {
            self.declare_shim_function(index, &specs, result_spec)
        } else {
            let params: Vec<ForeignType> = specs
                .iter()
                .copied()
                .map(scalar_of)
                .collect::<Result<_, _>>()?;
            self.declare_c_function(import.symbol(), &params, scalar_of(result_spec)?)
        };
        // The result the adapter itself stores. An aggregate result is written
        // through the caller's buffer by the shim, so there is nothing to store
        // and the out slot keeps the tag and pointer the caller put there.
        let stored_result = match result_spec {
            ForeignTypeSpec::Aggregate(_) => None,
            ForeignTypeSpec::Scalar(ty) => Some(ty),
        };

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
            let bad_result_slot =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"bad_result_slot".as_ptr());
            let cleanup =
                LLVMAppendBasicBlockInContext(context, adapter.value, c"cleanup".as_ptr());

            let args = LLVMGetParam(adapter.value, 0);
            let count = LLVMGetParam(adapter.value, 1);
            let out = LLVMGetParam(adapter.value, 2);

            // Entry: allocas for the status word and one slot per CString param.
            LLVMPositionBuilderAtEnd(builder, entry);
            let status = LLVMBuildAlloca(builder, types.i32, c"status".as_ptr());
            let mut cstring_slots = Vec::new();
            for (i, spec) in specs.iter().enumerate() {
                if *spec == ForeignType::CString {
                    let slot = LLVMBuildAlloca(builder, types.ptr, c"cstr".as_ptr());
                    LLVMBuildStore(builder, LLVMConstPointerNull(types.ptr), slot);
                    cstring_slots.push((i, slot));
                }
            }
            // Reference the version marker so a stale sidecar fails to link by
            // name; the call is inert.
            self.call_runtime(self.runtime.foreign_marker, &mut [], c"");
            let expected_count = LLVMConstInt(types.i32, specs.len() as u64, 0);
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
            for (i, spec) in specs.iter().enumerate() {
                let slot = self.bridge_element_ptr(args, i as u64);
                let tag = self.load_bridge_tag(slot);
                let expected = LLVMConstInt(types.i8, u64::from(spec.bridge_tag().0), 0);
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
            let mut c_args = Vec::with_capacity(specs.len() + 1);
            // An aggregate result's buffer pointer is the shim's first argument.
            // The caller wrote it into the out slot before the call, and the tag
            // check above has not covered it, so it is validated here.
            if result_spec.aggregate().is_some() {
                let payload = self.load_bridge_payload(out);
                let out_tag = self.load_bridge_tag(out);
                let tag_ok = LLVMBuildICmp(
                    builder,
                    LLVMIntPredicate::LLVMIntEQ,
                    out_tag,
                    LLVMConstInt(types.i8, u64::from(result_spec.bridge_tag().0), 0),
                    c"out.tag.ok".as_ptr(),
                );
                let buffer = LLVMBuildIntToPtr(builder, payload, types.ptr, c"out.buf".as_ptr());
                let not_null = LLVMBuildICmp(
                    builder,
                    LLVMIntPredicate::LLVMIntNE,
                    buffer,
                    LLVMConstPointerNull(types.ptr),
                    c"out.buf.ok".as_ptr(),
                );
                let slot_ok = LLVMBuildAnd(builder, tag_ok, not_null, c"out.ok".as_ptr());
                let ok =
                    LLVMAppendBasicBlockInContext(context, adapter.value, c"out.slot.ok".as_ptr());
                LLVMBuildCondBr(builder, slot_ok, ok, bad_result_slot);
                LLVMPositionBuilderAtEnd(builder, ok);
                c_args.push(buffer);
            }
            for (i, spec) in specs.iter().enumerate() {
                let slot = self.bridge_element_ptr(args, i as u64);
                let payload = self.load_bridge_payload(slot);
                if spec.aggregate().is_some() {
                    // Already C-layout bytes the caller owns for this call: the
                    // shim takes a pointer, so the payload word passes straight
                    // through with no conversion at all.
                    c_args.push(LLVMBuildIntToPtr(
                        builder,
                        payload,
                        types.ptr,
                        c"agg.ptr".as_ptr(),
                    ));
                } else if *spec == ForeignType::CString {
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
                    c_args.push(self.foreign_arg_to_c(payload, scalar_of(*spec)?)?);
                }
            }
            let rc = LLVMBuildCall2(
                builder,
                callee.ty,
                callee.value,
                c_args.as_mut_ptr(),
                c_args.len() as u32,
                match stored_result {
                    Some(ForeignType::Void) | None => c"".as_ptr(),
                    Some(_) => c"rc".as_ptr(),
                },
            );
            // Nothing to store for an aggregate: the shim wrote the bytes into
            // the caller's buffer, and the out slot already names it.
            if let Some(result) = stored_result {
                self.store_foreign_result(out, rc, result)?;
            }
            LLVMBuildStore(
                builder,
                LLVMConstInt(types.i32, u64::from(ForeignAdapterStatus::SUCCESS.0), 0),
                status,
            );
            LLVMBuildBr(builder, cleanup);

            // The caller's aggregate out slot was not a writable buffer.
            LLVMPositionBuilderAtEnd(builder, bad_result_slot);
            LLVMBuildStore(
                builder,
                LLVMConstInt(
                    types.i32,
                    u64::from(ForeignAdapterStatus::BAD_RESULT_SLOT.0),
                    0,
                ),
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
}
