//! Deep copy and drop of a value, mirroring the VM's `Heap::copy_value` and
//! `Heap::drop_value`.
//!
//! These live on [`Codegen`] rather than on one function's lowering because a
//! value's shape is a program-wide fact, not a per-body one: the same walk that
//! copies a local read also fills the clone/free *leaf* an array's runtime
//! helpers call ([`super::elements`]), and a leaf is emitted with no function
//! body in scope. Everything here needs is the builder, the runtime
//! declarations, and the program's type table.
//!
//! The walks are emitted into those leaves and nowhere else: a site calls the
//! leaf instead of expanding the walk again. See [`super::glue`], which is what
//! every site outside this module goes through.

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

    /// Whether a copied value must replace unique C-block handles in the copy.
    pub(super) fn owns_unique_c_storage(&self, ty: Type) -> bool {
        self.program.types.owns_unique_c_storage(ty)
    }

    /// Whether `ty` can reach C storage a retained call must transfer.
    pub(super) fn contains_c_storage(&self, ty: Type) -> bool {
        self.program.types.contains_c_storage(ty)
    }

    /// The element type of an array type.
    pub(super) fn element_of(&self, ty: Type) -> Result<Type, crate::LlvmError> {
        self.program
            .types
            .element_of(ty)
            .ok_or(crate::LlvmError::internal("an element of a non-array"))
    }

    /// Takes a share of everything the value at `at` owns, mirroring the VM's
    /// `Heap::copy_value`.
    ///
    /// # A copy is a retain
    ///
    /// Every arm below leaves the bits alone: a `String`, an array, an enum, an
    /// `Any` and a cell all copy by raising the share count of the object the
    /// handle already names, and a struct copies by doing that to each of its
    /// fields. So a copy never produces different bits from the ones it was
    /// given — the caller keeps the value it had, and this is only the counting.
    ///
    /// # By pointer, not by value
    ///
    /// A struct's field is reached with a `getelementptr` rather than by
    /// extracting it from the whole struct. A generated style struct is
    /// thousands of bytes, and at the code-generation level a development build
    /// uses, LLVM lowers a load of one into a move per field — so a walk that
    /// took the struct by value would spend more on loading it than on the
    /// counts it came to raise.
    ///
    /// The walk, not the site: this is emitted once per type, into that type's
    /// retain leaf. Fields go back through [`Codegen::retain_at`], so each
    /// field's own walk is a call to *its* leaf rather than more of this one.
    pub(super) fn retain_at_walk(
        &mut self,
        at: LLVMValueRef,
        ty: Type,
    ) -> Result<(), crate::LlvmError> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match ty {
            Type::CBlock => self.clone_cblock_at(at),
            Type::String => {
                let handle = self.load_handle(at, "str");
                self.copy_shared(handle, self.types.string_box, "str");
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
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let field = self.field_pointer(struct_type, at, index as u32);
                    if def.owns_c_storage_at(index as u32) {
                        self.clone_cblock_at(field)?;
                        continue;
                    }
                    if !self.owns_heap(field_ty) {
                        continue;
                    }
                    self.retain_at(field, field_ty)?;
                }
                Ok(())
            }
            // A copy takes a share of the array and walks nothing: the elements
            // are copied only if one of the two arrays is written, by the
            // runtime's mutable entry points, which is where the element clone
            // leaf goes instead. See `kira-native-bridge`'s `array` module.
            // Reading an array is most of what a frame does, and doing it
            // eagerly here was 78% of one.
            Type::Array(_) => {
                let handle = self.load_handle(at, "array");
                self.copy_shared(handle, self.types.array_header, "array");
                Ok(())
            }
            Type::Enum(_) => {
                let handle = self.load_handle(at, "enum");
                self.copy_shared(handle, self.types.enum_box, "enum");
                Ok(())
            }
            // An erased value copies exactly as an enum does, because its box
            // *is* an enum box: one more share of the same object. Nothing may
            // write through an `Any`, so two holders can observe nothing a deep
            // copy would have hidden — the same argument the enum arm rests on.
            Type::Any => {
                let handle = self.load_handle(at, "any");
                self.copy_shared(handle, self.types.enum_box, "any");
                Ok(())
            }
            // A cell copies as an enum does, because its box *is* an enum box —
            // and here the sharing is the point rather than an optimization
            // nobody can observe. A closure and the frame that declared the
            // `var` have to see each other's writes, so a copy must not be
            // independent. This is the one arm of this function that does not
            // preserve value semantics, because the type it copies does not
            // have them.
            Type::Cell(_) => {
                let handle = self.load_handle(at, "cell");
                self.copy_shared(handle, self.types.enum_box, "cell");
                Ok(())
            }
            // `owns_heap` is only true for the cases above.
            _ => Err(crate::LlvmError::internal("a copy of an unowned value")),
        }
    }

    /// Reads the handle a shared value is, out of the storage holding it.
    fn load_handle(&self, at: LLVMValueRef, name: &str) -> LLVMValueRef {
        let name = c_string(&format!("{name}.handle"));
        // SAFETY: `at` addresses storage holding a handle, which is a `ptr`,
        // and the builder is on a live block.
        unsafe { LLVMBuildLoad2(self.builder, self.types.ptr, at, name.as_ptr()) }
    }

    /// Replaces the C-block handle at `at` with an independent deep clone.
    fn clone_cblock_at(&mut self, at: LLVMValueRef) -> Result<(), crate::LlvmError> {
        // SAFETY: `at` addresses one live i64 C-block handle.
        let handle =
            unsafe { LLVMBuildLoad2(self.builder, self.types.i64, at, c"cblock".as_ptr()) };
        let clone = self.call(self.runtime.cblock_clone, &mut [handle], c"cblock.clone");
        // SAFETY: `at` is the destination slot and `clone` is its new handle.
        unsafe { LLVMBuildStore(self.builder, clone, at) };
        Ok(())
    }

    /// The address of field `index` inside the struct at `at`.
    pub(super) fn field_pointer(
        &self,
        struct_type: LLVMTypeRef,
        at: LLVMValueRef,
        index: u32,
    ) -> LLVMValueRef {
        let name = c_string(&format!("field.{index}.ptr"));
        // SAFETY: `at` addresses a value of `struct_type`, which has more than
        // `index` fields — the index came from that struct's own definition.
        unsafe { LLVMBuildStructGEP2(self.builder, struct_type, at, index, name.as_ptr()) }
    }

    /// Whether two values of `ty` are structurally equal, as an `i1`.
    ///
    /// Mirrors the VM's `Heap::values_equal`, and is reached the same way: only
    /// from an erasure, where both sides are already known to be the same Kira
    /// type. That is what lets this walk a struct field-by-field without
    /// checking anything about the operands first — the erasure box's tag
    /// settled it.
    ///
    /// Neither operand is consumed. A comparison reads and takes nothing, so a
    /// caller still owns both afterwards.
    ///
    /// By pointer for the same reason [`Codegen::retain_at_walk`] is: a struct's
    /// field is compared where it lies rather than by loading the struct around
    /// it twice.
    ///
    /// The walk, emitted into a type's equality leaf. A struct field goes back
    /// through [`Codegen::equal_at`], which is where the recursion becomes a
    /// call — see [`super::glue`].
    pub(super) fn equal_at_walk(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        let builder = self.builder;
        match ty {
            // A float compares as IEEE says, so `NaN` equals nothing: the same
            // rule `EqFloat` follows, and the VM's arm alongside it.
            Type::Float(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands are `double` and the builder is live.
                Ok(unsafe {
                    LLVMBuildFCmp(
                        builder,
                        llvm_sys::LLVMRealPredicate::LLVMRealOEQ,
                        a,
                        b,
                        c"eq.float".as_ptr(),
                    )
                })
            }
            Type::Int(_) | Type::Bool | Type::RawPtr | Type::ForeignPtr(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands share one integer type and the builder
                // is live.
                Ok(unsafe {
                    LLVMBuildICmp(
                        builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        a,
                        b,
                        c"eq.scalar".as_ptr(),
                    )
                })
            }
            // A cell has reference semantics, so identity *is* its equality:
            // two boxes holding equal values are still two places to write.
            // The same rule the VM applies (`Heap::objects_equal`), and it has
            // to be the same one — a captured `var` inside a struct reaches
            // here whenever that struct is erased, because erasing an aggregate
            // emits the equality leaf that walks it.
            Type::Cell(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: a cell is one opaque pointer on both sides and the
                // builder is live; `icmp eq` on two pointers is their identity.
                Ok(unsafe {
                    LLVMBuildICmp(
                        builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        a,
                        b,
                        c"eq.cell".as_ptr(),
                    )
                })
            }
            // The helper consumes what it compares, so each side is cloned for
            // it: the values themselves belong to whoever called this.
            Type::String => {
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(right, ty)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.str_eq, &mut [a, b], c"eq.str");
                Ok(self.truthy(equal))
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let field_types = self.field_types(id)?;
                // An empty struct is a value with nothing to disagree about.
                let mut all = self.const_bool(true);
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    let index = index as u32;
                    let (a, b) = (
                        self.field_pointer(struct_type, left, index),
                        self.field_pointer(struct_type, right, index),
                    );
                    let equal = self.equal_at(a, b, field_ty)?;
                    // SAFETY: both are `i1` and the builder is live. `and`
                    // rather than a branch chain: a field comparison has no
                    // side effect to skip, so there is nothing to short-circuit
                    // for beyond the work itself.
                    all = unsafe { LLVMBuildAnd(builder, all, equal, c"eq.field".as_ptr()) };
                }
                Ok(all)
            }
            // Both reach the runtime, which walks the elements or the tag and
            // payload. An array needs its element's leaf to compare items it
            // cannot type; an enum box carries everything its comparison needs.
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let eq = self.element_eq(element)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.array_eq, &mut [a, b, esize, eq], c"eq.array");
                Ok(self.truthy(equal))
            }
            Type::Enum(_) | Type::Any => {
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.any_eq, &mut [a, b], c"eq.enum");
                Ok(self.truthy(equal))
            }
            // Nothing else can be inside an erased value: `Void`, `Error`,
            // `CString`, a cell, a task, and callback state are all refused by
            // `Type::assignable_to` before `Any` takes them, and none is a
            // struct field type that could carry one in sideways.
            other => Err(crate::LlvmError::internal(format!(
                "an equality of `{other:?}`, which no erasure admits,"
            ))),
        }
    }

    /// Reads both sides of a comparison out of the storage holding them.
    fn load_operands(
        &self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<(LLVMValueRef, LLVMValueRef), crate::LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: both address a live value of `llvm_type` and the builder is
        // on a live block.
        Ok(unsafe {
            (
                LLVMBuildLoad2(self.builder, llvm_type, left, c"eq.a".as_ptr()),
                LLVMBuildLoad2(self.builder, llvm_type, right, c"eq.b".as_ptr()),
            )
        })
    }

    /// Releases whatever heap storage the value at `at` owns, mirroring the
    /// VM's `Heap::drop_value`.
    ///
    /// Emitted once per type into that type's free leaf, and by pointer,
    /// exactly as [`Codegen::retain_at_walk`] is.
    pub(super) fn release_at_walk(
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
    fn copy_shared(&mut self, value: LLVMValueRef, object: LLVMTypeRef, name: &str) {
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
    fn drop_shared(
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

    /// The address of a shared object's share count, the last field of each.
    fn shares_pointer(&self, value: LLVMValueRef, object: LLVMTypeRef, name: &str) -> LLVMValueRef {
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
            .ok_or(crate::LlvmError::internal(
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
            .ok_or(crate::LlvmError::internal(
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

    /// Narrows a runtime helper's `i8` answer to the `i1` Kira booleans are.
    pub(super) fn truthy(&self, value: LLVMValueRef) -> LLVMValueRef {
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
    pub(super) fn dynamic_alloca(
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
    pub(super) fn release_dynamic_alloca(&mut self, slot: LLVMValueRef, saved: LLVMValueRef) {
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
    pub(super) fn store_zero(
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
    pub(super) fn lifetime_start(&self, pointer: LLVMValueRef) {
        self.lifetime(pointer, c"llvm.lifetime.start.p0");
    }

    /// Ends the lifetime of a temporary allocation after its last use.
    pub(super) fn lifetime_end(&self, pointer: LLVMValueRef) {
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
