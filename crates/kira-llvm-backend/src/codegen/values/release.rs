//! Release walks and inline shared-object retain/release control flow.

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::Codegen;
use super::super::ffi::c_string;
use super::super::types::Callable;

impl Codegen<'_> {
    /// Releases whatever heap storage the value at `at` owns, mirroring the
    /// VM's `Heap::drop_value`.
    ///
    /// Emitted once per type into that type's free leaf, and by pointer,
    /// exactly as [`Codegen::retain_at_walk`] is.
    pub(in crate::codegen) fn release_at_walk(
        &mut self,
        at: LLVMValueRef,
        ty: Type,
    ) -> Result<(), crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::CBlock => {
                // SAFETY: `at` addresses one live i64 C-block handle.
                let handle =
                    unsafe { LLVMBuildLoad2(self.builder, self.types.i64, at, c"cblock".as_ptr()) };
                self.call(self.runtime.cblock_free, &mut [handle], c"");
                Ok(())
            }
            Type::String => {
                let last = self.runtime.str_free;
                let handle = self.load_handle(at, "str");
                self.drop_shared(handle, self.types.string_box, &mut [], last, "str");
                Ok(())
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let def = self
                    .program
                    .types
                    .structs()
                    .get(id)
                    .ok_or(crate::LlvmError::internal(
                        "a struct the module never declared",
                    ))?
                    .clone();
                // Before the members, which is the whole of the ordering rule:
                // the body reads the value it is being told about, so what it
                // holds is still there when it runs.
                if let Some(glue) = def.drop_glue {
                    self.call_drop_glue(at, glue)?;
                }
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let field = self.field_pointer(struct_type, at, index as u32);
                    if def.owns_c_storage_at(index as u32) {
                        // SAFETY: an owning C-layout slot contains one live or
                        // null C-block handle.
                        let handle = unsafe {
                            LLVMBuildLoad2(self.builder, self.types.i64, field, c"cblock".as_ptr())
                        };
                        self.call(self.runtime.cblock_free, &mut [handle], c"");
                        continue;
                    }
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    self.release_at(field, field_ty)?;
                }
                Ok(())
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let free = self.element_free(element)?;
                let last = self.runtime.array_free;
                let handle = self.load_handle(at, "array");
                self.drop_shared(
                    handle,
                    self.types.array_header,
                    &mut [esize, free],
                    last,
                    "array",
                );
                Ok(())
            }
            Type::Enum(_) => {
                let last = self.runtime.enum_free;
                let handle = self.load_handle(at, "enum");
                self.drop_shared(handle, self.types.enum_box, &mut [], last, "enum");
                Ok(())
            }
            // The box carries the payload kind that says what it owns, so
            // `kira_rt_enum_free` reclaims an erased `String` or struct without
            // this side having to remember which one was erased. That is the
            // whole reason the tag is written at the box rather than tracked in
            // the type: the free is driven by the value, as it is on the VM.
            Type::Any => {
                let last = self.runtime.enum_free;
                let handle = self.load_handle(at, "any");
                self.drop_shared(handle, self.types.enum_box, &mut [], last, "any");
                Ok(())
            }
            // The last release reclaims whatever the payload kind says the box
            // owns, exactly as an enum's does. A cell holding a closure that
            // captures the same cell is a cycle share counts cannot collect; it
            // leaks, and that is recorded in `kira-native-bridge`'s `cells`
            // module rather than defended against here.
            Type::Cell(_) => {
                let last = self.runtime.cell_free;
                let handle = self.load_handle(at, "cell");
                self.drop_shared(handle, self.types.enum_box, &mut [], last, "cell");
                Ok(())
            }
            _ => Err(crate::LlvmError::internal("a drop of an unowned value")),
        }
    }

    /// Copies a shared object: the same handle, held once more, emitted inline.
    ///
    /// The runtime helper is four instructions and generated code called it
    /// hundreds of thousands of times a frame, so the *call* was the cost.
    /// There is no slow path to fall back to — a copy is a count away from
    /// free — which is why this is emitted whole rather than as a fast path in
    /// front of a call, and why the object's layout is a type this module knows.
    pub(in crate::codegen) fn copy_shared(
        &mut self,
        value: LLVMValueRef,
        object: LLVMTypeRef,
        name: &str,
    ) {
        let function = self.current_function();
        let (bump, done) = (
            self.append_block(function, &c_string(&format!("{name}.copy.bump"))),
            self.append_block(function, &c_string(&format!("{name}.copy.end"))),
        );
        let counted = self.holds_a_count(value, object, name);
        // SAFETY: `counted` is an `i1` and both blocks belong to this function.
        unsafe { LLVMBuildCondBr(self.builder, counted, bump, done) };

        // SAFETY: `bump` is an empty block of the function being built.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, bump) };
        let shares = self.shares_pointer(value, object, name);
        // SAFETY: a counted handle addresses a live object, whose share count
        // this raises by one. It cannot wrap: it rises by one per live value.
        unsafe {
            let count = LLVMBuildLoad2(
                self.builder,
                self.types.usize_ty,
                shares,
                c_string(&format!("{name}.shares")).as_ptr(),
            );
            let one = LLVMConstInt(self.types.usize_ty, 1, 0);
            let raised = LLVMBuildAdd(
                self.builder,
                count,
                one,
                c_string(&format!("{name}.shares.up")).as_ptr(),
            );
            LLVMBuildStore(self.builder, raised, shares);
            LLVMBuildBr(self.builder, done);
            LLVMPositionBuilderAtEnd(self.builder, done);
        }
    }

    /// Releases one hold on a shared object, emitted inline, with the last
    /// release — the only one that frees anything — left to the runtime.
    ///
    /// `rest` is whatever else that last call takes after the handle: an
    /// element size and a free leaf for an array, nothing for an enum, which
    /// carries what it owns in the box.
    pub(in crate::codegen) fn drop_shared(
        &mut self,
        value: LLVMValueRef,
        object: LLVMTypeRef,
        rest: &mut [LLVMValueRef],
        release: Callable,
        name: &str,
    ) {
        let function = self.current_function();
        let (held, last, lower, done) = (
            self.append_block(function, &c_string(&format!("{name}.drop.held"))),
            self.append_block(function, &c_string(&format!("{name}.drop.last"))),
            self.append_block(function, &c_string(&format!("{name}.drop.lower"))),
            self.append_block(function, &c_string(&format!("{name}.drop.end"))),
        );
        let counted = self.holds_a_count(value, object, name);
        // SAFETY: `counted` is an `i1` and every block belongs to this function.
        unsafe { LLVMBuildCondBr(self.builder, counted, held, done) };

        // SAFETY: `held` is an empty block of the function being built.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, held) };
        let shares = self.shares_pointer(value, object, name);
        // SAFETY: a counted handle addresses a live object.
        unsafe {
            let count = LLVMBuildLoad2(
                self.builder,
                self.types.usize_ty,
                shares,
                c_string(&format!("{name}.shares")).as_ptr(),
            );
            let one = LLVMConstInt(self.types.usize_ty, 1, 0);
            let alone = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntULE,
                count,
                one,
                c_string(&format!("{name}.alone")).as_ptr(),
            );
            LLVMBuildCondBr(self.builder, alone, last, lower);

            // Somebody else still holds it, so only this claim on it goes.
            LLVMPositionBuilderAtEnd(self.builder, lower);
            let lowered = LLVMBuildSub(
                self.builder,
                count,
                one,
                c_string(&format!("{name}.shares.down")).as_ptr(),
            );
            LLVMBuildStore(self.builder, lowered, shares);
            LLVMBuildBr(self.builder, done);

            // The last release is where the storage goes, which is the
            // runtime's job — it knows what the object owns.
            LLVMPositionBuilderAtEnd(self.builder, last);
        }
        let mut args = Vec::with_capacity(rest.len() + 1);
        args.push(value);
        args.extend_from_slice(rest);
        self.call(release, &mut args, c"");
        // SAFETY: `done` is a block of the function being built.
        unsafe {
            LLVMBuildBr(self.builder, done);
            LLVMPositionBuilderAtEnd(self.builder, done);
        }
    }

    /// Whether a handle names an object with a share count in it.
    ///
    /// A null handle names nothing. An enum handle may also carry a
    /// payload-less variant's whole value in the handle itself, marked by its
    /// low bit, and a copy of one of those is itself — see
    /// `kira_native_bridge::enums::is_inline`. Testing that bit costs an
    /// instruction an array would not need, and it is emitted for both because
    /// a header from the allocator never has it set, so the answer is the same
    /// either way and the two paths stay one.
    pub(in crate::codegen) fn holds_a_count(
        &self,
        value: LLVMValueRef,
        _object: LLVMTypeRef,
        name: &str,
    ) -> LLVMValueRef {
        // SAFETY: `value` is a handle (a `ptr`) and the builder is on a live
        // block.
        unsafe {
            let bits = LLVMBuildPtrToInt(
                self.builder,
                value,
                self.types.i64,
                c_string(&format!("{name}.bits")).as_ptr(),
            );
            let zero = LLVMConstInt(self.types.i64, 0, 0);
            let one = LLVMConstInt(self.types.i64, 1, 0);
            let marker = LLVMBuildAnd(
                self.builder,
                bits,
                one,
                c_string(&format!("{name}.inline.bit")).as_ptr(),
            );
            let is_object = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                marker,
                zero,
                c_string(&format!("{name}.not.inline")).as_ptr(),
            );
            let is_live = LLVMBuildICmp(
                self.builder,
                llvm_sys::LLVMIntPredicate::LLVMIntNE,
                bits,
                zero,
                c_string(&format!("{name}.not.null")).as_ptr(),
            );
            LLVMBuildAnd(
                self.builder,
                is_object,
                is_live,
                c_string(&format!("{name}.counted")).as_ptr(),
            )
        }
    }

    /// The address of a shared object's share count, the last field of each.
    pub(in crate::codegen) fn shares_pointer(
        &self,
        value: LLVMValueRef,
        object: LLVMTypeRef,
        name: &str,
    ) -> LLVMValueRef {
        // Each object says where its own count sits: a string's follows the two
        // words of the `Box<[u8]>` it owns, an array header's and an enum box's
        // follow three fields. The layout test beside each type is what holds
        // it there.
        let shares_field = if object == self.types.string_box {
            kira_runtime_abi::STRING_SHARES_FIELD
        } else {
            kira_runtime_abi::ENUM_BOX_SHARES_FIELD
        };
        // SAFETY: the caller has established `value` addresses a live object of
        // this layout, whose share count is at the index chosen above.
        unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                object,
                value,
                shares_field,
                c_string(&format!("{name}.shares.ptr")).as_ptr(),
            )
        }
    }
}
