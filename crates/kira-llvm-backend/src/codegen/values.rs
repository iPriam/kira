//! Deep copy and drop of a value, mirroring the VM's `Heap::copy_value` and
//! `Heap::drop_value`.
//!
//! These live on [`Codegen`] rather than on one function's lowering because a
//! value's shape is a program-wide fact, not a per-body one: the same walk that
//! copies a local read also fills the clone/free *leaf* an array's runtime
//! helpers call ([`super::elements`]), and a leaf is emitted with no function
//! body in scope. Everything here needs is the builder, the runtime
//! declarations, and the program's type table.

use kira_semantics_model::{StructId, Type};
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::ffi::c_string;
use super::types::Callable;

impl Codegen<'_> {
    /// Whether a value of `ty` owns heap storage that a copy must clone and a
    /// drop must release.
    pub(super) fn owns_heap(&self, ty: Type) -> bool {
        self.program.types.owns_heap(ty)
    }

    /// The element type of an array type.
    pub(super) fn element_of(&self, ty: Type) -> Result<Type, crate::LlvmError> {
        self.program
            .types
            .element_of(ty)
            .ok_or(crate::LlvmError::Unsupported("an element of a non-array"))
    }

    /// Produces an independent copy of `value`, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// Deep, field by field: a struct's copy clones every string it reaches, so
    /// no two live values share a handle and neither drop frees the other's.
    /// Scalars and structs of scalars copy for free — LLVM's `insertvalue`
    /// chain folds away — so the walk only costs anything where it must.
    pub(super) fn copy_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(value);
        }
        match ty {
            Type::String => Ok(self.call(self.runtime.str_clone, &mut [value], c"str.copy")),
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                let mut copy = value;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32);
                    let copied = self.copy_value(field, field_ty)?;
                    copy = self.insert_field(copy, copied, index as u32);
                }
                Ok(copy)
            }
            // A copy takes a share of the array and walks nothing: the elements
            // are copied only if one of the two arrays is written, by the
            // runtime's mutable entry points, which is where the element clone
            // leaf goes instead. See `kira-native-bridge`'s `array` module.
            // Reading an array is most of what a frame does, and doing it
            // eagerly here was 78% of one.
            Type::Array(_) => self.copy_shared(value, self.types.array_header, "array"),
            Type::Enum(_) => self.copy_shared(value, self.types.enum_box, "enum"),
            // `owns_heap` is only true for the cases above.
            _ => Err(crate::LlvmError::Unsupported("a copy of an unowned value")),
        }
    }

    /// Releases whatever heap storage `value` owns, mirroring the VM's
    /// `Heap::drop_value`.
    pub(super) fn drop_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<(), crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::String => {
                self.call(self.runtime.str_free, &mut [value], c"");
                Ok(())
            }
            Type::Struct(id) => {
                let field_types = self.field_types(id)?;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    let field = self.extract_field(value, index as u32);
                    self.drop_value(field, field_ty)?;
                }
                Ok(())
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let free = self.element_free(element)?;
                let last = self.runtime.array_free;
                self.drop_shared(
                    value,
                    self.types.array_header,
                    &mut [esize, free],
                    last,
                    "array",
                )
            }
            Type::Enum(_) => {
                let last = self.runtime.enum_free;
                self.drop_shared(value, self.types.enum_box, &mut [], last, "enum")
            }
            _ => Err(crate::LlvmError::Unsupported("a drop of an unowned value")),
        }
    }

    /// Copies a shared object: the same handle, held once more, emitted inline.
    ///
    /// The runtime helper is four instructions and generated code called it
    /// hundreds of thousands of times a frame, so the *call* was the cost.
    /// There is no slow path to fall back to — a copy is a count away from
    /// free — which is why this is emitted whole rather than as a fast path in
    /// front of a call, and why the object's layout is a type this module knows.
    fn copy_shared(
        &mut self,
        value: LLVMValueRef,
        object: LLVMTypeRef,
        name: &str,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
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
        Ok(value)
    }

    /// Releases one hold on a shared object, emitted inline, with the last
    /// release — the only one that frees anything — left to the runtime.
    ///
    /// `rest` is whatever else that last call takes after the handle: an
    /// element size and a free leaf for an array, nothing for an enum, which
    /// carries what it owns in the box.
    fn drop_shared(
        &mut self,
        value: LLVMValueRef,
        object: LLVMTypeRef,
        rest: &mut [LLVMValueRef],
        release: Callable,
        name: &str,
    ) -> Result<(), crate::LlvmError> {
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
        Ok(())
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
    fn holds_a_count(&self, value: LLVMValueRef, _object: LLVMTypeRef, name: &str) -> LLVMValueRef {
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

    /// The address of a shared object's share count, the last field of both.
    fn shares_pointer(&self, value: LLVMValueRef, object: LLVMTypeRef, name: &str) -> LLVMValueRef {
        // SAFETY: the caller has established `value` addresses a live object of
        // this layout, whose fourth field is the share count — the index both
        // `ARRAY_HEADER_SHARES_FIELD` and `ENUM_BOX_SHARES_FIELD` name, and
        // which the layout tests beside each type hold them to.
        unsafe {
            LLVMBuildStructGEP2(
                self.builder,
                object,
                value,
                kira_runtime_abi::ENUM_BOX_SHARES_FIELD,
                c_string(&format!("{name}.shares.ptr")).as_ptr(),
            )
        }
    }

    /// The function currently being built.
    fn current_function(&self) -> LLVMValueRef {
        // SAFETY: a value is only ever copied or dropped inside a function
        // body or a leaf, so the builder is positioned inside one.
        unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) }
    }

    /// Appends a fresh block to `function`.
    fn append_block(&self, function: LLVMValueRef, name: &std::ffi::CStr) -> LLVMBasicBlockRef {
        // SAFETY: `function` is a live function in this module's context.
        unsafe { LLVMAppendBasicBlockInContext(self.context, function, name.as_ptr()) }
    }

    /// The payload type of one enum variant, or an error when it has none.
    ///
    /// Only called for an [`kira_ir::IrExpr::EnumNew`] that carries a payload,
    /// so a payload-less variant here is a broken IR contract, not user input.
    pub(super) fn enum_payload_type(
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
            .ok_or(crate::LlvmError::Unsupported(
                "an enum payload the program never declared",
            ))
    }

    /// The field types of a declared struct.
    pub(super) fn field_types(&self, id: StructId) -> Result<Vec<Type>, crate::LlvmError> {
        self.program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.iter().map(|field| field.ty).collect())
            .ok_or(crate::LlvmError::Unsupported(
                "a struct the program never declared",
            ))
    }

    /// Reads field `index` out of a struct *value*.
    pub(super) fn extract_field(&self, value: LLVMValueRef, index: u32) -> LLVMValueRef {
        let name = c_string(&format!("field.{index}"));
        // SAFETY: `value` is a struct value with more than `index` fields — the
        // index came from that struct's own definition — and the builder is on
        // a live block.
        unsafe { LLVMBuildExtractValue(self.builder, value, index, name.as_ptr()) }
    }

    /// Returns `value` with field `index` replaced by `field`.
    pub(super) fn insert_field(
        &self,
        value: LLVMValueRef,
        field: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("with.{index}"));
        // SAFETY: as `extract_field`, and `field` has field `index`'s type.
        unsafe { LLVMBuildInsertValue(self.builder, value, field, index, name.as_ptr()) }
    }

    /// Emits a call to a runtime helper from within the current block.
    pub(super) fn call(
        &self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: the builder is on a live block and every call site supplies
        // arguments matching the callable's declared signature.
        unsafe { self.call_runtime(callable, args, name) }
    }
}
