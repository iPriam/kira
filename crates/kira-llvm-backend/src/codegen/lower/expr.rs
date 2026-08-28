//! Expression lowering: constants, local reads, and every operator.
//!
//! The arithmetic here is where native code and the interpreter agree or fail
//! to: wrapping integer ops, a trapping division, and short-circuit operators
//! that are control flow rather than instructions.

use kira_ir::{IrExpr, IrExprId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::ffi::c_string;
use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers an expression to a value.
    pub(in crate::codegen) fn lower_expr(
        &mut self,
        id: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        match self.codegen.program.expr(id).clone() {
            IrExpr::Int(value) => Ok(self.codegen.const_int(value)),
            IrExpr::Float(value) => Ok(self.codegen.const_float(value)),
            IrExpr::Bool(value) => Ok(self.codegen.const_bool(value)),
            IrExpr::Str(text) => {
                let data = self.codegen.string_constant(&text);
                // The length is a `usize` at the target's width, not the host's.
                let length = self.codegen.const_usize(text.len() as u64);
                Ok(self.call(self.codegen.runtime.str_new, &mut [data, length], c"str"))
            }
            // A `RawPtr` is an `i64` here, so the null pointer is that zero.
            IrExpr::RawPtrNull => Ok(self.codegen.const_int(0)),
            IrExpr::ForeignCallbackPtr { callback } => {
                self.codegen.callback_thunk_address(callback as usize)
            }
            IrExpr::CellNull { .. } => {
                // A null cell is only closure-representation padding.
                // SAFETY: the pointer type belongs to this live module context.
                Ok(unsafe { LLVMConstNull(self.codegen.types.ptr) })
            }
            IrExpr::Local(slot) => self.load_local(slot),
            IrExpr::CellNew { value, ty } => self.lower_cell_new(value, ty),
            IrExpr::CellGet { slot, ty } => self.lower_cell_get(slot, ty),
            IrExpr::Unary { op, operand } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_unary(op, value))
            }
            IrExpr::Binary { op, lhs, rhs } => self.lower_binary(op, lhs, rhs),
            IrExpr::Select {
                cond,
                then,
                otherwise,
                ty,
            } => self.lower_select(cond, then, otherwise, ty),
            IrExpr::Call {
                callee,
                args,
                writebacks,
                result,
                ..
            } => self.lower_call(callee, &args, &writebacks, result),
            IrExpr::StructNew { struct_id, fields } => self.lower_struct_new(struct_id, &fields),
            IrExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => self.lower_enum_new(enum_id, tag, payload),
            IrExpr::IntoAny { value, from } => self.lower_into_any(value, from),
            IrExpr::Widen { value, from, to } => self.lower_widen(value, from, to),
            IrExpr::EnumTag { value } => self.lower_enum_tag(value),
            IrExpr::EnumPayload { value, ty } => self.lower_enum_payload(value, ty),
            IrExpr::Field { base, index, ty } => self.lower_field(base, index, ty),
            IrExpr::MathOperation { op, operands } => self.lower_math_operation(op, &operands),
            IrExpr::ScalarText { value } => self.lower_scalar_text(value),
            IrExpr::ArrayElements { value, element } => self.lower_array_elements(value, element),
            IrExpr::ForeignField {
                base,
                aggregate,
                member,
                ty,
            } => self.lower_foreign_field(base, aggregate, member, ty),
            IrExpr::ForeignMemberAddress {
                base,
                aggregate,
                member,
                ..
            } => self.lower_foreign_member_address(base, aggregate, member),
            IrExpr::ForeignElement {
                base,
                aggregate,
                index,
                ..
            } => self.lower_foreign_element(base, aggregate, index),
            IrExpr::ArrayNew { ty, elements } => self.lower_array_new(ty, &elements),
            IrExpr::Index { base, index, ty } => self.lower_index(base, index, ty),
            IrExpr::TaskOp { prim, operands } => self.lower_task_op(prim, operands),
            IrExpr::ArrayLen { array } => self.lower_array_len(array),
            IrExpr::StringLen { text } => self.lower_string_len(text),
            IrExpr::StringCharAt { text, index } => self.lower_string_char_at(text, index),
            IrExpr::StringSubstring { text, start, end } => {
                self.lower_string_substring(text, start, end)
            }
            IrExpr::StringIndexOf { text, needle } => self.lower_string_index_of(text, needle),
            IrExpr::StringOperation {
                op,
                text,
                ref arguments,
                ..
            } => self.lower_string_operation(op, text, arguments.clone()),
            IrExpr::StringOf { value } => self.lower_string_of(value),
            IrExpr::CLayoutAddress { value, aggregate } => {
                self.lower_clayout_address(value, aggregate)
            }
            IrExpr::CStringNew { text } => {
                let value = self.lower_expr(text)?;
                Ok(self.call(
                    self.codegen.runtime.cblock_text,
                    &mut [value],
                    c"cblock.text",
                ))
            }
            IrExpr::FileSystem { op, args, ty } => self.lower_file_system(op, &args, ty),
            IrExpr::Compiler { op, args, ty } => self.lower_compiler(op, &args, ty),
            IrExpr::Env { op, args, .. } => self.lower_env(op, &args),
            IrExpr::ArrayAppend { place, value } => self.lower_array_append(&place, value),
            IrExpr::NativeState { value, type_id, .. } => {
                self.lower_native_state_new(value, type_id)
            }
            IrExpr::NativeUserData { state } => self.lower_expr(state),
            IrExpr::NativeRecover { raw, type_id, ty } => {
                self.lower_native_recover_value(raw, type_id, ty)
            }
            IrExpr::NativeStateFree { token } => self.lower_native_state_free(token),
            IrExpr::Convert { operand, kind, .. } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_convert(kind, value))
            }
        }
    }

    /// Reads a value of `ty` out of `slot`, taking a share of what it owns.
    ///
    /// The share is taken **through the slot**, before the read: a copy raises
    /// counts and changes no bits, so the value loaded afterwards is the copy.
    /// Retaining first is what keeps a large struct to a single load — spilling
    /// the loaded value back into a scratch slot for the walk would double it.
    pub(super) fn read_owned(
        &mut self,
        slot: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if self.codegen.owns_unique_c_storage(ty) {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `slot` addresses a live value of `llvm_type`.
            let value = unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    llvm_type,
                    slot,
                    c"owned.source".as_ptr(),
                )
            };
            return self.copy_value(value, ty);
        }
        self.codegen.retain_at(slot, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `slot` addresses a live value of `llvm_type` and the builder
        // is on a live block.
        Ok(unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, slot, c"owned".as_ptr()) })
    }

    /// The handle a place holds, read without copying what holds it.
    ///
    /// `None` when the expression is not a place this can address, and the
    /// general route — evaluate, use, drop — handles it.
    pub(super) fn borrowed_local_handle(
        &mut self,
        base: IrExprId,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let Some((pointer, ty)) = self.borrowed_place_pointer(base)? else {
            return Ok(None);
        };
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `pointer` addresses a live value of `llvm_type`; the handle is
        // read, not copied, and is not freed here because this expression does
        // not own it.
        Ok(Some(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                llvm_type,
                pointer,
                c"place.borrow".as_ptr(),
            )
        }))
    }

    /// The address a place expression names, and the type stored there.
    ///
    /// A place is anything reached by naming a local and walking into it —
    /// `xs`, `tree.nodes`, `rows[i].cells`. Every step of that walk is address
    /// arithmetic, so the whole path can be addressed rather than evaluated,
    /// and a handle at the end of it read without cloning what holds it.
    ///
    /// This lets `tree.nodes[i]` address the array in place instead of cloning
    /// every element before reading one entry.
    ///
    /// `None` for anything that is not such a place: the caller then evaluates
    /// the expression, uses it, and drops it as before.
    pub(super) fn borrowed_place_pointer(
        &mut self,
        expr: IrExprId,
    ) -> Result<Option<(LLVMValueRef, Type)>, LlvmError> {
        match *self.codegen.program.expr(expr) {
            IrExpr::Local(slot) => {
                let ty = self.local_type(slot)?;
                // A native-state local holds a token, and the value it names
                // lives in the box behind it — which is addressable, so a place
                // rooted here is addressed through the box's payload.
                if let Some(type_id) = self
                    .function
                    .native_state_locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                {
                    if !self.state_is_boxed() {
                        return Ok(None);
                    }
                    let payload = self.recover_native_state_alloca(slot, type_id, ty)?.0;
                    return Ok(Some((payload, ty)));
                }
                Ok(Some((self.local_pointer(slot)?, ty)))
            }
            IrExpr::Field { base, index, ty } => {
                let Some((pointer, base_ty)) = self.borrowed_place_pointer(base)? else {
                    return Ok(None);
                };
                if !matches!(base_ty, Type::Struct(_)) {
                    return Ok(None);
                }
                let struct_type = self.codegen.llvm_type(base_ty)?;
                let name = c_string(&format!("place.field.{index}.ptr"));
                // SAFETY: `pointer` addresses a value of `struct_type`, and
                // `index` came from that struct's own definition.
                let field = unsafe {
                    LLVMBuildStructGEP2(
                        self.codegen.builder,
                        struct_type,
                        pointer,
                        index,
                        name.as_ptr(),
                    )
                };
                Ok(Some((field, ty)))
            }
            IrExpr::Index { base, index, ty } => {
                let Some(handle) = self.borrowed_local_handle(base)? else {
                    return Ok(None);
                };
                Ok(Some((self.element_slot(handle, index, ty)?, ty)))
            }
            _ => Ok(None),
        }
    }

    /// Turns an array handle into the address of element `index` **to read**,
    /// bounds-checked by the runtime.
    ///
    /// The item block behind the handle may be shared with another array, so
    /// nothing may be written through the address this gives back;
    /// [`Self::element_slot_mut`] is the one that may.
    pub(super) fn element_slot(
        &mut self,
        array: LLVMValueRef,
        index: IrExprId,
        element: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let index_value = self.lower_expr(index)?;
        let esize = self.codegen.abi_size(element)?;
        Ok(self.call(
            self.codegen.runtime.array_slot,
            &mut [array, index_value, esize],
            c"slot",
        ))
    }

    /// Turns the **slot holding** an array into the address of element `index`
    /// to write, bounds-checked by the runtime.
    ///
    /// Copying an array only takes a share of it, so a write is where the
    /// copying actually happens: the runtime gives this slot an array of its
    /// own first, cloning each element with the leaf handed over here, and
    /// stores the fresh handle back. That is why this takes the slot rather
    /// than the handle — a split *replaces* the handle, and whatever holds it
    /// has to see that.
    ///
    /// Every write into an array goes through this — a store, an append, and a
    /// step of a place walk that passes through one — and each of them already
    /// starts from a place, so the slot costs nothing to supply.
    pub(super) fn element_slot_mut(
        &mut self,
        holder: LLVMValueRef,
        index: IrExprId,
        element: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let index_value = self.lower_expr(index)?;
        let esize = self.codegen.abi_size(element)?;
        let clone = self.codegen.element_clone(element)?;
        Ok(self.call(
            self.codegen.runtime.array_slot_mut,
            &mut [holder, index_value, esize, clone],
            c"slot.mut",
        ))
    }

    /// An array's element count (`xs.count`), the VM's `ArrayLen`.
    /// One deferred-task primitive: the native mirror of the VM's `TaskOp`.
    ///
    /// The operands are lowered left to right, which is the order the VM pushes
    /// them, so a program whose arguments have side effects orders those side
    /// effects identically on both engines.
    fn lower_task_op(
        &mut self,
        prim: kira_runtime_abi::TaskPrim,
        operands: [IrExprId; 3],
    ) -> Result<LLVMValueRef, LlvmError> {
        let tag = self.codegen.const_int(i64::from(prim.as_byte()));
        let first = self.lower_expr(operands[0])?;
        let second = self.lower_expr(operands[1])?;
        let third = self.lower_expr(operands[2])?;
        Ok(self.call(
            self.codegen.runtime.task_op,
            &mut [tag, first, second, third],
            c"task",
        ))
    }

    /// A string's character count (`s.count`), the VM's `StringLen`.
    ///
    /// The helper consumes the string, which is the lowering convention for
    /// every operation that reads one — so there is nothing to drop here.
    fn lower_string_len(&mut self, text: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        Ok(self.call(self.codegen.runtime.str_count, &mut [value], c"s.count"))
    }

    /// The byte at an index of a string (`s.charAt(i)`), the VM's
    /// `StringCharAt`.
    ///
    /// The helper consumes the string, which is the lowering convention for
    /// every operation that reads one, and traps on an out-of-range index
    /// rather than answering — the same trap the VM raises.
    fn lower_string_char_at(
        &mut self,
        text: IrExprId,
        index: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let at = self.lower_expr(index)?;
        Ok(self.call(
            self.codegen.runtime.str_char_at,
            &mut [value, at],
            c"s.charAt",
        ))
    }

    /// A half-open byte slice of a string (`s.substring(a, b)`), the VM's
    /// `StringSubstring`.
    fn lower_string_substring(
        &mut self,
        text: IrExprId,
        start: IrExprId,
        end: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let from = self.lower_expr(start)?;
        let to = self.lower_expr(end)?;
        Ok(self.call(
            self.codegen.runtime.str_substring,
            &mut [value, from, to],
            c"s.substring",
        ))
    }

    /// The first byte index of a needle (`s.indexOf(n)`), the VM's
    /// `StringIndexOf`.
    fn lower_string_index_of(
        &mut self,
        text: IrExprId,
        needle: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let value = self.lower_expr(text)?;
        let pattern = self.lower_expr(needle)?;
        Ok(self.call(
            self.codegen.runtime.str_index_of,
            &mut [value, pattern],
            c"s.indexOf",
        ))
    }

    /// One of the shared-opcode string operations, the VM's `StringOp`.
    ///
    /// The operand byte indexes the callable table, so a new operation needs a
    /// row there and nothing here — the receiver and arguments are pushed in
    /// source order whatever the operation is, and each helper frees every
    /// handle it was given.
    fn lower_string_operation(
        &mut self,
        op: kira_runtime_abi::StringOp,
        text: IrExprId,
        arguments: Vec<IrExprId>,
    ) -> Result<LLVMValueRef, LlvmError> {
        let mut operands = Vec::with_capacity(arguments.len() + 1);
        operands.push(self.lower_expr(text)?);
        for argument in arguments {
            operands.push(self.lower_expr(argument)?);
        }
        let callable = self.codegen.runtime.string_ops[usize::from(op.as_byte())];
        Ok(self.call(callable, &mut operands, c"s.stringOp"))
    }

    /// A scalar rendered as text (`String(x)`), the VM's `StringOf`.
    ///
    /// The operand's static type picks the helper, so each one formats a value
    /// it already knows the shape of — which is what keeps the rendering
    /// byte-identical to the one `print` gives on this backend.
    fn lower_string_of(&mut self, value: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.type_of(value);
        let operand = self.lower_expr(value)?;
        let callable = match ty {
            Type::Bool => self.codegen.runtime.str_of_bool,
            Type::Float(_) => self.codegen.runtime.str_of_float,
            Type::String => return Ok(operand),
            _ => self.codegen.runtime.str_of_int,
        };
        Ok(self.call(callable, &mut [operand], c"String"))
    }

    /// Reads a local slot, copying what it holds.
    ///
    /// The VM's `LoadLocal` copies the value, so the slot keeps ownership of
    /// its own storage and the reader owns an independent copy.
    fn load_local(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.local_type(slot)?;
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(slot as usize)
            .copied()
            .flatten()
        {
            return self.load_native_state_local(slot, type_id, ty);
        }
        let pointer = self.local_pointer(slot)?;
        let value = self.read_owned(pointer, ty)?;
        // A value that runs a user `Drop` is never copied — binding one moves
        // (`TypeTable::moves_on_bind`), so the checker has already refused a
        // second use of this local. Reading it therefore *takes* it: the local
        // no longer holds anything, and the release at the end of the frame
        // must not run a body the value's new owner will run.
        // Only a user-`Drop` value is moved by an ordinary read. Other
        // heap-backed values are copied by `read_owned`, so their local keeps
        // owning the original share and must remain eligible for a later
        // scope or frame release.
        if self
            .local_type(slot)
            .is_ok_and(|ty| self.codegen.program.types.runs_user_drop(ty))
        {
            self.clear_live_flag(slot);
        }
        Ok(value)
    }

    /// Lowers `expr` in a position that does not consume it.
    ///
    /// Only a local read differs: a local whose type runs a user `Drop` is
    /// *taken* by an ordinary read, and a borrowed position leaves the caller
    /// holding the value. Everywhere else the value is a temporary the position
    /// owns either way.
    pub(super) fn lower_borrowed_expr(
        &mut self,
        expr: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let IrExpr::Local(slot) = *self.codegen.program.expr(expr) else {
            return self.lower_expr(expr);
        };
        let value = self.lower_expr(expr)?;
        // A borrowed read of an ordinary heap value leaves its local live; the
        // flag only tracks the move-sensitive user-`Drop` case.
        if self
            .local_type(slot)
            .is_ok_and(|ty| self.codegen.program.types.runs_user_drop(ty))
        {
            self.set_live_flag(slot);
        }
        Ok(value)
    }

    /// Marks a local whose type runs a user `Drop` as holding a value again,
    /// undoing the take an ordinary read performed.
    fn set_live_flag(&mut self, slot: u32) {
        let Some(flag) = self.live_flag(slot) else {
            return;
        };
        // SAFETY: `flag` addresses an `i1` in this function's entry block and
        // the builder is on a live block.
        unsafe {
            LLVMBuildStore(
                self.codegen.builder,
                LLVMConstInt(self.codegen.types.i1, 1, 0),
                flag,
            );
        }
    }

    /// Marks a local whose type runs a user `Drop` as no longer holding a
    /// value. A local of any other type has no flag and this does nothing.
    fn clear_live_flag(&mut self, slot: u32) {
        let Some(flag) = self.live_flag(slot) else {
            return;
        };
        // SAFETY: `flag` addresses an `i1` in this function's entry block and
        // the builder is on a live block.
        unsafe {
            LLVMBuildStore(
                self.codegen.builder,
                LLVMConstInt(self.codegen.types.i1, 0, 0),
                flag,
            );
        }
    }

    /// Builds a struct value from its fields.
    ///
    /// The fields arrive in declaration order with every one present — analysis
    /// filled the defaults — so this is a straight `insertvalue` chain onto a
    /// zeroed value, with no reordering and no gaps.
    fn lower_struct_new(
        &mut self,
        struct_id: kira_semantics_model::StructId,
        fields: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = Type::Struct(struct_id);
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` is this struct's type in this live context.
        let mut value = unsafe { LLVMGetUndef(llvm_type) };
        for (index, &field) in fields.iter().enumerate() {
            let mut lowered = self.lower_expr(field)?;
            if self.c_storage_slot(ty, index)? && self.type_of(field) != Type::CBlock {
                lowered = self.call(
                    self.codegen.runtime.cblock_alien,
                    &mut [lowered],
                    c"struct.cblock.alien",
                );
            }
            value = self.insert_field(value, lowered, index as u32)?;
        }
        Ok(value)
    }

    /// Reads one field out of a struct expression.
    ///
    /// The field is copied out *before* the base is dropped, because the base
    /// owns the storage the field names — handing it out without copying would
    /// hand out exactly what the drop is about to free. This is the VM's
    /// `GetField` instruction, in the same order.
    fn lower_field(
        &mut self,
        base: IrExprId,
        index: u32,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        // A base that names storage is read through it. Lowering it as a value
        // first would load the whole struct to reach one field of it, then copy
        // and drop everything else in it — and a generated style struct is
        // thousands of bytes, which at a development build's code-generation
        // level is a move per field, three times over, for one read.
        let base_ty = self.type_of(base);
        if matches!(base_ty, Type::Struct(_))
            && let Some(pointer) = self.addressable(base)?
        {
            let struct_type = self.codegen.llvm_type(base_ty)?;
            let field = self.codegen.field_pointer(struct_type, pointer, index);
            if self.c_storage_slot(base_ty, index as usize)? {
                // SAFETY: an owning C-layout slot contains one live or null
                // i64 C-block handle.
                let handle = unsafe {
                    LLVMBuildLoad2(
                        self.codegen.builder,
                        self.codegen.types.i64,
                        field,
                        c"field.cblock".as_ptr(),
                    )
                };
                return Ok(self.call(
                    self.codegen.runtime.cblock_word,
                    &mut [handle],
                    c"field.cblock.word",
                ));
            }
            return self.read_owned(field, ty);
        }
        // Reading a member does not consume the value it is read from.
        let base_value = self.lower_borrowed_expr(base)?;
        let field = self.extract_field(base_value, index)?;
        if self.c_storage_slot(base_ty, index as usize)? {
            let word = self.call(
                self.codegen.runtime.cblock_word,
                &mut [field],
                c"field.cblock.word",
            );
            self.drop_value(base_value, base_ty)?;
            return Ok(word);
        }
        let copy = self.copy_value(field, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// The storage `expr` names, when it names storage this frame can address.
    ///
    /// A local slot, or a struct field of one, however deeply nested. Nothing
    /// else: an expression that computes a value has no address, and a place
    /// behind an array or an enum is reached through the runtime rather than
    /// through a `getelementptr` on this frame.
    ///
    /// Reading through the address is only sound because nothing between the
    /// walk and the read can write the slot — the walk is a chain of
    /// `getelementptr`, which evaluates nothing.
    fn addressable(&mut self, expr: IrExprId) -> Result<Option<LLVMValueRef>, LlvmError> {
        match *self.codegen.program.expr(expr) {
            IrExpr::Local(slot) => {
                // A callback-state local holds a token rather than the value,
                // and a written-through parameter is already a pointer the
                // caller owns; both are read through their own paths.
                if self
                    .function
                    .native_state_locals
                    .get(slot as usize)
                    .copied()
                    .flatten()
                    .is_some()
                {
                    return Ok(None);
                }
                Ok(Some(self.local_pointer(slot)?))
            }
            IrExpr::Field { base, index, .. } => {
                let base_ty = self.type_of(base);
                if !matches!(base_ty, Type::Struct(_)) {
                    return Ok(None);
                }
                let Some(pointer) = self.addressable(base)? else {
                    return Ok(None);
                };
                let struct_type = self.codegen.llvm_type(base_ty)?;
                Ok(Some(self.codegen.field_pointer(
                    struct_type,
                    pointer,
                    index,
                )))
            }
            // An element of a borrowable array is storage this frame reaches
            // too: the runtime hands back the element's own slot, and a walk
            // into it is address arithmetic like every other step. Reading
            // through it is what keeps `rows[i].cell.tag` from copying the
            // element out — and a copy of a value that runs a user `Drop` would
            // run its body when the copy died, which is a body the reader never
            // asked for.
            IrExpr::Index {
                base: array,
                index: at,
                ty: element,
            } => {
                let Some(handle) = self.borrowed_local_handle(array)? else {
                    return Ok(None);
                };
                Ok(Some(self.element_slot(handle, at, element)?))
            }
            _ => Ok(None),
        }
    }
}
