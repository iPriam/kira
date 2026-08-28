//! Value-shape helpers, dynamic temporaries, and zero initialization.

use kira_semantics_model::{StructId, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::Codegen;
use super::super::ffi::c_string;
use super::super::types::Callable;

impl Codegen<'_> {
    /// The function currently being built.
    pub(in crate::codegen) fn current_function(&self) -> LLVMValueRef {
        // SAFETY: a value is only ever copied or dropped inside a function
        // body or a leaf, so the builder is positioned inside one.
        unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) }
    }

    /// Appends a fresh block to `function`.
    pub(in crate::codegen) fn append_block(
        &self,
        function: LLVMValueRef,
        name: &std::ffi::CStr,
    ) -> LLVMBasicBlockRef {
        // SAFETY: `function` is a live function in this module's context.
        unsafe { LLVMAppendBasicBlockInContext(self.context, function, name.as_ptr()) }
    }

    /// The payload type of one enum variant, or an error when it has none.
    ///
    /// Only called for an [`kira_ir::IrExpr::EnumNew`] that carries a payload,
    /// so a payload-less variant here is a broken IR contract, not user input.
    pub(in crate::codegen) fn enum_payload_type(
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
            .ok_or(crate::LlvmError::internal(
                "an enum payload the program never declared",
            ))
    }

    /// The field types of a declared struct.
    pub(in crate::codegen) fn field_types(
        &self,
        id: StructId,
    ) -> Result<Vec<Type>, crate::LlvmError> {
        self.program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.iter().map(|field| field.ty).collect())
            .ok_or(crate::LlvmError::internal(
                "a struct the program never declared",
            ))
    }

    /// Reads field `index` out of a struct *value*.
    pub(in crate::codegen) fn extract_field(
        &self,
        value: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("field.{index}"));
        // SAFETY: `value` is a struct value with more than `index` fields — the
        // index came from that struct's own definition — and the builder is on
        // a live block.
        unsafe { LLVMBuildExtractValue(self.builder, value, index, name.as_ptr()) }
    }

    /// Returns `value` with field `index` replaced by `field`.
    pub(in crate::codegen) fn insert_field(
        &self,
        value: LLVMValueRef,
        field: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("with.{index}"));
        // SAFETY: as `extract_field`, and `field` has field `index`'s type.
        unsafe { LLVMBuildInsertValue(self.builder, value, field, index, name.as_ptr()) }
    }

    /// Narrows a runtime helper's `i8` answer to the `i1` Kira booleans are.
    pub(in crate::codegen) fn truthy(&self, value: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: the helper returns an `i8` of 0 or 1, and the builder is on
        // a live block.
        unsafe {
            let zero = LLVMConstInt(self.types.i8, 0, 0);
            LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntNE,
                value,
                zero,
                c"truthy".as_ptr(),
            )
        }
    }

    /// Emits a call to a runtime helper from within the current block.
    pub(in crate::codegen) fn call(
        &self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: the builder is on a live block and every call site supplies
        // arguments matching the callable's declared signature.
        unsafe { self.call_runtime(callable, args, name) }
    }

    /// Allocates one temporary value with a runtime-sized alloca.
    ///
    /// A plain alloca contributes its full type size to the enclosing native
    /// function's static frame even when it lives in a mutually-exclusive
    /// dispatcher arm.  These temporaries are only needed on the selected arm,
    /// so make the element count genuinely dynamic; LLVM then adjusts the
    /// stack at the point of execution instead of reserving every arm's
    /// payload in every call frame.  The count is one or two elements and the
    /// second element is intentionally unused.
    ///
    /// Returns the slot together with the stack pointer saved just before it,
    /// which [`Self::release_dynamic_alloca`] gives back. A dynamic alloca
    /// lowers to a runtime stack adjustment, so one executed in a loop
    /// reserves its bytes again on every iteration until something restores —
    /// pairing every allocation with that restore is what keeps a per-frame
    /// loop from walking the native stack to its limit.
    pub(in crate::codegen) fn dynamic_alloca(
        &self,
        llvm_type: LLVMTypeRef,
        name: &std::ffi::CStr,
    ) -> (LLVMValueRef, LLVMValueRef) {
        // SAFETY: the stack-save intrinsic, integer conversions, and alloca
        // use types from this module's context and the builder is on a live
        // block.
        unsafe {
            let mut no_args = [];
            let saved = self.call(self.runtime.stack_save, &mut no_args, c"temporary.stack");
            let bits = LLVMBuildPtrToInt(
                self.builder,
                saved,
                self.types.i64,
                c"temporary.stack.bits".as_ptr(),
            );
            let low_bit = LLVMBuildAnd(
                self.builder,
                bits,
                LLVMConstInt(self.types.i64, 1, 0),
                c"temporary.count.bit".as_ptr(),
            );
            let count = LLVMBuildAdd(
                self.builder,
                low_bit,
                LLVMConstInt(self.types.i64, 1, 0),
                c"temporary.count".as_ptr(),
            );
            let slot = LLVMBuildArrayAlloca(self.builder, llvm_type, count, name.as_ptr());
            (slot, saved)
        }
    }

    /// Gives back the native stack a [`Self::dynamic_alloca`] reserved.
    ///
    /// Ends the slot's lifetime first, then restores the saved pointer. Every
    /// read of the slot must happen before this runs — the restore makes the
    /// bytes behind it dead by construction.
    pub(in crate::codegen) fn release_dynamic_alloca(
        &mut self,
        slot: LLVMValueRef,
        saved: LLVMValueRef,
    ) {
        self.lifetime_end(slot);
        self.call(self.runtime.stack_restore, &mut [saved], c"");
    }

    /// The largest zero a first-class store is still the cheaper way to write.
    ///
    /// Two machine words: a `String` handle, a `(ptr, len)` pair, a small
    /// struct of scalars. Past that, an aggregate store is lowered field by
    /// field and a `memset` is one instruction whatever the size.
    const INLINE_ZERO_BYTES: u64 = 16;

    /// Writes `ty`'s zero over the storage at `pointer`.
    ///
    /// A struct's zero is all-zero bytes — that is what `LLVMConstNull` means
    /// for every field type Kira puts in one — so a large struct is zeroed with
    /// a `memset` rather than with a store of the constant. LLVM lowers an
    /// aggregate store field by field, and a generated UI body declares
    /// hundreds of style structs: the prologue alone reached a quarter of a
    /// megabyte of `movq $0`, which is code LLVM has to select, allocate, and
    /// emit before the function does anything at all.
    pub(in crate::codegen) fn store_zero(
        &self,
        pointer: LLVMValueRef,
        zero: LLVMValueRef,
        llvm_type: LLVMTypeRef,
    ) {
        // SAFETY: `llvm_type` belongs to this module's context, whose data
        // layout was set when the module was created.
        let size = unsafe { llvm_sys::target::LLVMABISizeOfType(self.target_data, llvm_type) };
        if size <= Self::INLINE_ZERO_BYTES {
            // SAFETY: `zero` has `llvm_type` and `pointer` addresses storage
            // for it; the builder is on a live block.
            unsafe { LLVMBuildStore(self.builder, zero, pointer) };
            return;
        }
        // SAFETY: as above, plus `pointer` addresses `size` bytes — it is an
        // allocation of exactly `llvm_type` — and the alignment is the one LLVM
        // gives that type on this target.
        unsafe {
            let align = llvm_sys::target::LLVMABIAlignmentOfType(self.target_data, llvm_type);
            let byte = LLVMConstInt(self.types.i8, 0, 0);
            let length = LLVMConstInt(self.types.i64, size, 0);
            LLVMBuildMemSet(self.builder, pointer, byte, length, align);
        }
    }

    /// Marks a temporary allocation as live for LLVM's stack slot colouring.
    ///
    /// Synthesized construct dispatchers contain one temporary for every
    /// possible family variant, but only one arm can execute. Plain `alloca`
    /// gives LLVM function-long lifetime semantics, so a large family made
    /// every nested dispatch reserve the sum of all arm payloads. The lifetime
    /// intrinsics make the mutually-exclusive scope explicit without changing
    /// ownership or the generated ABI.
    pub(in crate::codegen) fn lifetime_start(&self, pointer: LLVMValueRef) {
        self.lifetime(pointer, c"llvm.lifetime.start.p0");
    }

    /// Ends the lifetime of a temporary allocation after its last use.
    pub(in crate::codegen) fn lifetime_end(&self, pointer: LLVMValueRef) {
        self.lifetime(pointer, c"llvm.lifetime.end.p0");
    }

    fn lifetime(&self, pointer: LLVMValueRef, name: &std::ffi::CStr) {
        // SAFETY: LLVM 22 spells the opaque-pointer lifetime declarations
        // `llvm.lifetime.{start,end}.p0` with the exact `void(ptr)` signature;
        // both the declaration and argument belong to this live module/context.
        unsafe {
            // LLVM 22 removed the size operand from these intrinsics.  The
            // default-address-space overload has the fixed signature
            // `void (ptr)` and the `.p0` suffix is part of its canonical name.
            let mut params = [self.types.ptr];
            let function_type =
                LLVMFunctionType(self.types.void, params.as_mut_ptr(), params.len() as u32, 0);
            // Registering the canonical intrinsic name with its exact LLVM 22
            // signature is more robust than asking the C API to infer an
            // overload for a non-overloaded intrinsic.  The verifier still
            // recognizes the declaration by name and applies the intrinsic's
            // lifetime semantics.
            let declaration = {
                let existing = LLVMGetNamedFunction(self.module, name.as_ptr());
                if existing.is_null() {
                    LLVMAddFunction(self.module, name.as_ptr(), function_type)
                } else {
                    existing
                }
            };
            let mut args = [pointer];
            LLVMBuildCall2(
                self.builder,
                function_type,
                declaration,
                args.as_mut_ptr(),
                args.len() as u32,
                c"".as_ptr(),
            );
        }
    }
}
