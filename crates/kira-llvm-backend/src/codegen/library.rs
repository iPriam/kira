//! The literal "extern C in Kira": the symbols a native Kira library exports.
//!
//! Three kinds, and every one of them shares the uniform trampoline shape the
//! hybrid seam already load-tests:
//!
//! ```text
//! void <symbol>(const BridgeValue *args, u32 count, BridgeValue *out)
//! ```
//!
//! One C-ABI shape for every Kira signature, never a per-signature typed C
//! symbol. A typed symbol per export would re-open ABI drift — the caller and
//! the callee would have to agree on a struct-passing convention per signature,
//! compiled separately, with nothing but a name to catch a disagreement — to buy
//! nothing the generated wrapper does not already hide.
//!
//! # A handle is a box
//!
//! Inside native code a class instance is an LLVM struct value that dies with
//! its frame. A handle outlives the call by definition, so an exported function
//! returning one *moves* the value into a box ([`kira_rt_box_new`]) and hands
//! back the box's address. The class's synthesized destructor drops whatever the
//! instance owned and then frees the box. Nothing else ever frees one, which is
//! the boundary contract's "Kira allocates, only the generated destructor
//! frees".
//!
//! [`kira_rt_box_new`]: https://docs.rs/kira-native-bridge
//!
//! A handle *argument* is lent, so the trampoline deep-copies the boxed value
//! before the call: the callee owns its parameters and drops them at return, and
//! a callee dropping the consumer's button would free strings the consumer still
//! holds a handle to.
//!
//! # Allocation failure is a typed error, not a crash
//!
//! `kira_rt_box_new` returns null when the allocator cannot serve the request.
//! Storing through that pointer would be undefined behavior in generated code,
//! so a failed box writes `Void` into the result slot instead. The consumer's
//! wrapper reads a tag it did not expect and reports it — the same
//! `unexpected result` path the VM engine already has. A library never gets to
//! end its caller's process, and generated code is held to that too.

use kira_semantics_model::{StructId, Type};
use llvm_sys::LLVMLinkage;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::ffi::c_string;
use crate::LlvmError;
use crate::exports::{NativeClass, NativeExport};

/// The three parameters every trampoline takes, and the pieces of one being
/// built.
struct Trampoline {
    /// The trampoline function itself.
    value: LLVMValueRef,
    /// `args`: the packed arguments the consumer supplied.
    args: LLVMValueRef,
    /// `out`: where the result is written.
    out: LLVMValueRef,
}

impl Codegen<'_> {
    /// Emits the whole export surface: the marker, one trampoline per export,
    /// and one destructor per exported class.
    pub(super) fn lower_export_surface(&mut self) -> Result<(), LlvmError> {
        if let Some(marker) = self.exports.abi_marker.clone() {
            self.lower_abi_marker(&marker);
        }
        for export in self.exports.functions.clone() {
            self.lower_export_trampoline(&export)?;
        }
        for class in self.exports.classes.clone() {
            self.lower_class_destructor(&class)?;
        }
        Ok(())
    }

    /// Emits the per-library ABI marker: an empty function the wrapper calls.
    ///
    /// It is not quite empty. The body references the *runtime* ABI marker, the
    /// way a C `main` does, so a library carries both guards: one for what a
    /// `kira_rt_*` helper owns, one for what a `kira_lib_*` trampoline's
    /// arguments mean. An executable gets the runtime guard from its `main`; a
    /// library has no `main`, and without this it would have had no guard at
    /// all.
    fn lower_abi_marker(&mut self, symbol: &str) {
        let name = c_string(symbol);
        // SAFETY: the types belong to this live module and the block is appended
        // to the function just created, before anything is built into it.
        unsafe {
            let signature = LLVMFunctionType(self.types.void, std::ptr::null_mut(), 0, 0);
            let marker = LLVMAddFunction(self.module, name.as_ptr(), signature);
            let block = LLVMAppendBasicBlockInContext(self.context, marker, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
            self.call_runtime(self.runtime.abi_marker, &mut [], c"");
            LLVMBuildRetVoid(self.builder);
        }
    }

    /// Emits one exported function's trampoline.
    fn lower_export_trampoline(&mut self, export: &NativeExport) -> Result<(), LlvmError> {
        let index = export.function as usize;
        let function = self
            .program
            .functions
            .get(index)
            .ok_or(LlvmError::internal("an export naming no function"))?;
        let target = self.functions[index].ok_or(LlvmError::internal(
            "an export whose function has no native body",
        ))?;
        let (return_type, param_count) = (function.return_type, function.param_count);
        let mut param_types = Vec::with_capacity(param_count as usize);
        for slot in 0..param_count {
            param_types.push(
                function
                    .param_type(slot)
                    .ok_or(LlvmError::internal("a parameter with no type"))?,
            );
        }

        let tramp = self.begin_trampoline(&export.symbol);
        let mut lowered = Vec::with_capacity(param_types.len());
        for (slot, ty) in param_types.iter().copied().enumerate() {
            let element = self.argument_slot(&tramp, slot as u32);
            lowered.push(self.read_export_argument(element, ty)?);
        }

        let returns_value = return_type != Type::Void;
        let name = if returns_value { c"result" } else { c"" };
        // SAFETY: `lowered` matches the target's declared signature — both were
        // built from the same `IrFunction` — and the builder is on the
        // trampoline's entry block.
        let result = unsafe {
            LLVMBuildCall2(
                self.builder,
                target.ty,
                target.value,
                lowered.as_mut_ptr(),
                lowered.len() as u32,
                name.as_ptr(),
            )
        };
        self.write_export_result(&tramp, result, return_type)?;
        Ok(())
    }

    /// Emits one exported class's destructor.
    ///
    /// It drops whatever the boxed instance owns — every string, array, and
    /// nested value the class reaches, through the same walk a local's drop
    /// uses — and then frees the box. A null handle is a no-op: a destructor
    /// reached with a handle that was never made must not crash.
    fn lower_class_destructor(&mut self, class: &NativeClass) -> Result<(), LlvmError> {
        let id = self.class_id(&class.name)?;
        let ty = Type::Struct(id);
        let llvm_type = self.llvm_type(ty)?;
        let size = self.abi_size(ty)?;

        let tramp = self.begin_trampoline(&class.symbol);
        let element = self.argument_slot(&tramp, 0);
        let boxed = self.read_handle_payload(element);

        // SAFETY: every block below belongs to the trampoline just created, and
        // the builder is positioned on one of them before each instruction.
        let (live, done) = unsafe {
            (
                LLVMAppendBasicBlockInContext(self.context, tramp.value, c"live".as_ptr()),
                LLVMAppendBasicBlockInContext(self.context, tramp.value, c"done".as_ptr()),
            )
        };
        // SAFETY: `boxed` is a pointer value and the builder is on the entry
        // block, which is unterminated.
        unsafe {
            let null = LLVMConstNull(self.types.ptr);
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                boxed,
                null,
                c"handle.null".as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, done, live);
            LLVMPositionBuilderAtEnd(self.builder, live);
        }

        // SAFETY: on this path `boxed` is a live box holding one `llvm_type`.
        let value = unsafe { LLVMBuildLoad2(self.builder, llvm_type, boxed, c"boxed".as_ptr()) };
        self.drop_value(value, ty)?;
        self.call(self.runtime.box_free, &mut [boxed, size], c"");
        // SAFETY: both blocks are unterminated and belong to this trampoline.
        unsafe {
            LLVMBuildBr(self.builder, done);
            LLVMPositionBuilderAtEnd(self.builder, done);
        }
        self.write_bridge_value(tramp.out, std::ptr::null_mut(), Type::Void)?;
        // SAFETY: the builder is on the `done` block, which is unterminated.
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(())
    }

    /// The struct this class name denotes, or a typed error naming it.
    ///
    /// Unreachable through a surface this compiler derived from the same
    /// program, and checked anyway: the surface arrives as plain data, and a
    /// backend that trusted it would emit a destructor for a type that does not
    /// exist.
    fn class_id(&self, name: &str) -> Result<StructId, LlvmError> {
        self.program
            .types
            .structs()
            .lookup(name)
            .ok_or(LlvmError::internal(
                "an exported class the program never declared",
            ))
    }

    /// Declares one trampoline and positions the builder on its entry block.
    fn begin_trampoline(&mut self, symbol: &str) -> Trampoline {
        let name = c_string(symbol);
        let types = self.types;
        // SAFETY: every type belongs to this live module, `params` outlives the
        // `LLVMFunctionType` call, and the block is appended to the function
        // just created.
        unsafe {
            let mut params = [types.ptr, types.i32, types.ptr];
            let signature =
                LLVMFunctionType(types.void, params.as_mut_ptr(), params.len() as u32, 0);
            let value = LLVMAddFunction(self.module, name.as_ptr(), signature);
            // External on purpose: this is the artifact's whole visible surface.
            LLVMSetLinkage(value, LLVMLinkage::LLVMExternalLinkage);
            let block = LLVMAppendBasicBlockInContext(self.context, value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, block);
            Trampoline {
                value,
                args: LLVMGetParam(value, 0),
                out: LLVMGetParam(value, 2),
            }
        }
    }

    /// The address of argument `slot` in the packed argument array.
    ///
    /// `count` is not consulted, exactly as the hybrid trampoline does not
    /// consult it: the consumer builds the call from the same surface this was
    /// generated from, so a mismatch is a broken artifact rather than a runtime
    /// condition — and the marker is what catches a broken artifact, at the
    /// link, by name.
    fn argument_slot(&self, tramp: &Trampoline, slot: u32) -> LLVMValueRef {
        // SAFETY: `args` points at an array of at least `slot + 1`
        // `BridgeValue`s, and the builder is on a live block.
        unsafe {
            let mut offset = [LLVMConstInt(self.types.i32, u64::from(slot), 0)];
            LLVMBuildInBoundsGEP2(
                self.builder,
                self.types.bridge_value,
                tramp.args,
                offset.as_mut_ptr(),
                1,
                c"arg.slot".as_ptr(),
            )
        }
    }

    /// Reads a `BridgeValue`'s payload as a pointer.
    fn read_handle_payload(&self, slot: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: `slot` points at a `BridgeValue` and the builder is live.
        unsafe {
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                self.types.bridge_value,
                slot,
                2,
                c"handle.payload.ptr".as_ptr(),
            );
            let payload = LLVMBuildLoad2(
                self.builder,
                self.types.i64,
                payload_ptr,
                c"handle.payload".as_ptr(),
            );
            LLVMBuildIntToPtr(
                self.builder,
                payload,
                self.types.ptr,
                c"handle.box".as_ptr(),
            )
        }
    }

    /// Lowers one argument the consumer packed into the value the body takes.
    ///
    /// A handle is the only one that is not the hybrid seam's own reader: it is
    /// lent, so the boxed instance is loaded and deep-copied, leaving the
    /// consumer's object untouched by whatever the callee does to its parameter.
    fn read_export_argument(
        &mut self,
        slot: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let Type::Struct(_) = ty else {
            return self.read_bridge_payload(slot, ty);
        };
        let llvm_type = self.llvm_type(ty)?;
        let boxed = self.read_handle_payload(slot);
        // SAFETY: the consumer's handle names a live box holding one
        // `llvm_type`; the wrapper's `Drop` is what makes that true, and a
        // handle is unforgeable in the safe API it is reached through.
        let value = unsafe { LLVMBuildLoad2(self.builder, llvm_type, boxed, c"lent".as_ptr()) };
        self.copy_value(value, ty)
    }

    /// Writes one result back, boxing it first when it is a handle.
    fn write_export_result(
        &mut self,
        tramp: &Trampoline,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), LlvmError> {
        let Type::Struct(_) = ty else {
            self.write_bridge_value(tramp.out, value, ty)?;
            // SAFETY: the builder is on the trampoline's unterminated block.
            unsafe { LLVMBuildRetVoid(self.builder) };
            return Ok(());
        };

        let size = self.abi_size(ty)?;
        let boxed = self.call(self.runtime.box_new, &mut [size], c"handle.box");
        // SAFETY: both blocks belong to the trampoline being built.
        let (stored, failed) = unsafe {
            (
                LLVMAppendBasicBlockInContext(self.context, tramp.value, c"boxed".as_ptr()),
                LLVMAppendBasicBlockInContext(self.context, tramp.value, c"unboxed".as_ptr()),
            )
        };
        // SAFETY: `boxed` is a pointer and the builder is on an unterminated
        // block of this trampoline.
        unsafe {
            let null = LLVMConstNull(self.types.ptr);
            let is_null = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                boxed,
                null,
                c"box.null".as_ptr(),
            );
            LLVMBuildCondBr(self.builder, is_null, failed, stored);
            LLVMPositionBuilderAtEnd(self.builder, stored);
            LLVMBuildStore(self.builder, value, boxed);
        }
        self.write_handle_value(tramp.out, boxed);
        // SAFETY: the builder is on `stored`, which is unterminated.
        unsafe {
            LLVMBuildRetVoid(self.builder);
            LLVMPositionBuilderAtEnd(self.builder, failed);
        }
        // The allocation failed, so there is nowhere to put the value; drop what
        // it owns rather than leaking it, and report a result the consumer's
        // wrapper will refuse by name.
        self.drop_value(value, ty)?;
        self.write_bridge_value(tramp.out, std::ptr::null_mut(), Type::Void)?;
        // SAFETY: the builder is on `failed`, which is unterminated.
        unsafe { LLVMBuildRetVoid(self.builder) };
        Ok(())
    }

    /// Writes a boxed instance's address into the `BridgeValue` at `slot`,
    /// tagged `HANDLE`.
    ///
    /// Written here rather than through [`Codegen::write_bridge_value`] because
    /// that routine maps a *Kira type* onto a tag, and a struct has no crossing
    /// tag there by design — the `@Native`/`@Runtime` seam refuses one. This is
    /// the export boundary, where a struct crosses as a handle to storage the
    /// library owns, which is a different question with a different answer.
    fn write_handle_value(&self, slot: LLVMValueRef, boxed: LLVMValueRef) {
        let types = self.types;
        // SAFETY: `slot` points at a writable `BridgeValue`, `boxed` is a
        // pointer, and the builder is on a live block.
        unsafe {
            let tag_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                0,
                c"handle.tag.ptr".as_ptr(),
            );
            LLVMBuildStore(
                self.builder,
                LLVMConstInt(
                    types.i8,
                    u64::from(kira_runtime_abi::BridgeValueTag::HANDLE.0),
                    0,
                ),
                tag_ptr,
            );
            let payload_ptr = LLVMBuildStructGEP2(
                self.builder,
                types.bridge_value,
                slot,
                2,
                c"handle.out.ptr".as_ptr(),
            );
            let payload =
                LLVMBuildPtrToInt(self.builder, boxed, types.i64, c"handle.bits".as_ptr());
            LLVMBuildStore(self.builder, payload, payload_ptr);
        }
    }
}
