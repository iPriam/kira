//! Expression lowering: constants, local reads, and every operator.
//!
//! The arithmetic here is where native code and the interpreter agree or fail
//! to: wrapping integer ops, a trapping division, and short-circuit operators
//! that are control flow rather than instructions.

use kira_ir::{IrExpr, IrExprId, IrPlace};
use kira_runtime_abi::EnumPayloadKind;
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
            IrExpr::Local(slot) => self.load_local(slot),
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
            IrExpr::EnumTag { value } => self.lower_enum_tag(value),
            IrExpr::EnumPayload { value, ty } => self.lower_enum_payload(value, ty),
            IrExpr::Field { base, index, ty } => self.lower_field(base, index, ty),
            IrExpr::ArrayNew { ty, elements } => self.lower_array_new(ty, &elements),
            IrExpr::Index { base, index, ty } => self.lower_index(base, index, ty),
            IrExpr::ArrayLen { array } => self.lower_array_len(array),
            IrExpr::StringLen { text } => self.lower_string_len(text),
            IrExpr::StringCharAt { text, index } => self.lower_string_char_at(text, index),
            IrExpr::StringSubstring { text, start, end } => {
                self.lower_string_substring(text, start, end)
            }
            IrExpr::StringIndexOf { text, needle } => self.lower_string_index_of(text, needle),
            IrExpr::StringOf { value } => self.lower_string_of(value),
            IrExpr::CLayoutAddress { value, aggregate } => {
                self.lower_clayout_address(value, aggregate)
            }
            IrExpr::CStringNew { text } => {
                let value = self.lower_expr(text)?;
                Ok(self.call(self.codegen.runtime.cstring_retain, &mut [value], c"cstr"))
            }
            IrExpr::FileSystem { op, args, ty } => self.lower_file_system(op, &args, ty),
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

    /// Builds an array from its written elements and leaves its handle.
    ///
    /// Allocate full, then fill: `array_new` reserves exactly this many slots,
    /// so each element is written through `array_slot` at a constant, in-range
    /// index — the bounds check the runtime does there can never fire here. The
    /// slots are fresh, so a plain store suffices; there is no prior value to
    /// drop, unlike a store into a live element.
    ///
    /// The read slot rather than the mutable one, even though this writes: the
    /// item block was allocated a few instructions ago and no other array has
    /// ever seen it, so there is nothing for a copy to protect.
    fn lower_array_new(
        &mut self,
        ty: Type,
        elements: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let element = self.codegen.element_of(ty)?;
        let count = self.codegen.const_int(elements.len() as i64);
        let esize = self.codegen.abi_size(element)?;
        let handle = self.call(
            self.codegen.runtime.array_new,
            &mut [count, esize],
            c"array",
        );
        for (index, &value) in elements.iter().enumerate() {
            // Elements evaluate left to right, as the VM pushes them.
            let lowered = self.lower_expr(value)?;
            let at = self.codegen.const_int(index as i64);
            let esize = self.codegen.abi_size(element)?;
            let slot = self.call(
                self.codegen.runtime.array_slot,
                &mut [handle, at, esize],
                c"slot",
            );
            // SAFETY: `slot` points at a fresh element slot of `element`'s type
            // and `lowered` has that type; the builder is on a live block.
            unsafe { LLVMBuildStore(self.codegen.builder, lowered, slot) };
        }
        Ok(handle)
    }

    /// Reads one element out of an array (`xs[i]`).
    ///
    /// The element is copied out — the array owns it, so handing it out
    /// unshared means copying it first — and that copy is what preserves value
    /// semantics. The *base* does not need copying at all.
    ///
    /// # Reading an element does not copy the array
    ///
    /// A general base expression is evaluated, indexed, and dropped. A base
    /// that is just a local is **borrowed** instead: its handle is read without
    /// a clone and never freed here, because this expression does not own it.
    ///
    /// Cloning it would make one element read cost the whole array, so a loop
    /// over `n` elements would cost `O(n²)` — reading 200,000 elements took
    /// seven seconds before this, and loading an 18 MB mesh never finished.
    fn lower_index(
        &mut self,
        base: IrExprId,
        index: IrExprId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let Some(handle) = self.borrowed_local_handle(base)? {
            let slot = self.element_slot(handle, index, ty)?;
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `slot` points at a live element of `llvm_type`, bounds
            // checked by the runtime, and the builder is on a live block.
            let element =
                unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, slot, c"elem".as_ptr()) };
            return self.copy_value(element, ty);
        }
        let base_ty = self.type_of(base);
        let base_value = self.lower_expr(base)?;
        let slot = self.element_slot(base_value, index, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `slot` points at a live element of `llvm_type`, bounds-checked
        // by the runtime, and the builder is on a live block.
        let element =
            unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, slot, c"elem".as_ptr()) };
        let copy = self.copy_value(element, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// The handle a place holds, read without copying what holds it.
    ///
    /// `None` when the expression is not a place this can address, and the
    /// general route — evaluate, use, drop — handles it.
    fn borrowed_local_handle(&mut self, base: IrExprId) -> Result<Option<LLVMValueRef>, LlvmError> {
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
    /// This is what keeps `tree.nodes[i]` from cloning `nodes`. Reading a field
    /// yields a *copy* of it, so an array reached through one used to be
    /// duplicated in full — every element, and every handle inside every
    /// element — to read one entry of it and drop the duplicate again. A layout
    /// pass does that thousands of times a frame.
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

    /// Turns an array handle into the address of element `index` **to write**,
    /// bounds-checked by the runtime.
    ///
    /// Copying an array only takes a share of its item block, so a write is
    /// where the copying actually happens: the runtime gives this handle a
    /// block of its own first, cloning each element with the leaf handed over
    /// here. Every write into an array goes through this — a store, an append,
    /// and a step of a place walk that passes through one — and that is the
    /// whole of what keeps the sharing invisible.
    pub(super) fn element_slot_mut(
        &mut self,
        array: LLVMValueRef,
        index: IrExprId,
        element: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let index_value = self.lower_expr(index)?;
        let esize = self.codegen.abi_size(element)?;
        let clone = self.codegen.element_clone(element)?;
        Ok(self.call(
            self.codegen.runtime.array_slot_mut,
            &mut [array, index_value, esize, clone],
            c"slot.mut",
        ))
    }

    /// An array's element count (`xs.count`), the VM's `ArrayLen`.
    fn lower_array_len(&mut self, array: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let array_ty = self.type_of(array);
        let array_value = self.lower_expr(array)?;
        let len = self.call(self.codegen.runtime.array_len, &mut [array_value], c"len");
        self.drop_value(array_value, array_ty)?;
        Ok(len)
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

    /// Appends one element to the array a place names (`xs.append(v)`), yielding
    /// `Void`.
    ///
    /// The VM's `ArrayAppend`, in the same order: the place's index expressions
    /// are evaluated first, then the value, and only then is the slot reserved —
    /// so a value that reads the array (`xs.append(xs.count)`) sees the length
    /// from before the push, as the VM's evaluate-then-append order does. The
    /// slot is fresh, so a plain store lands the value with nothing to drop.
    fn lower_array_append(
        &mut self,
        place: &IrPlace,
        value: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let Some(type_id) = self
            .function
            .native_state_locals
            .get(place.local as usize)
            .copied()
            .flatten()
        {
            let root_ty = self.local_type(place.local)?;
            // For a boxed state the array being appended to lives in the
            // state's own storage, so the push reaches it directly.
            let (root, write_back) =
                self.recover_native_state_alloca(place.local, type_id, root_ty)?;
            let mut slot = root;
            let mut ty = root_ty;
            for step in &place.path {
                (slot, ty) = self.walk_place_step(slot, ty, step)?;
            }
            let element = self.codegen.element_of(ty)?;
            // SAFETY: `slot` holds an array handle.
            let handle = unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    self.codegen.types.ptr,
                    slot,
                    c"array".as_ptr(),
                )
            };
            let lowered = self.lower_expr(value)?;
            let esize = self.codegen.abi_size(element)?;
            let clone = self.codegen.element_clone(element)?;
            let element_slot = self.call(
                self.codegen.runtime.array_push_slot,
                &mut [handle, esize, clone],
                c"push",
            );
            // SAFETY: `element_slot` is one fresh element slot.
            unsafe { LLVMBuildStore(self.codegen.builder, lowered, element_slot) };
            if write_back {
                self.write_back_native_state(place.local, type_id, root_ty, root)?;
            }
            return Ok(self.codegen.const_bool(false));
        }
        // Every step is a walk: the place names the array itself, and the walk
        // lands on the slot that *holds* its handle.
        let (slot, ty) = self.walk_place(place.local, &place.path)?;
        let element = self.codegen.element_of(ty)?;
        // SAFETY: `slot` holds an array handle (a `ptr`); the builder is live.
        let handle = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                slot,
                c"array".as_ptr(),
            )
        };
        let lowered = self.lower_expr(value)?;
        let esize = self.codegen.abi_size(element)?;
        // Appending is a write, so the runtime gives this handle an item block
        // of its own — with the leaf cloning each element it carries over —
        // before the new element lands in it.
        let clone = self.codegen.element_clone(element)?;
        let element_slot = self.call(
            self.codegen.runtime.array_push_slot,
            &mut [handle, esize, clone],
            c"push",
        );
        // SAFETY: `element_slot` is a fresh, uninitialized element slot of
        // `element`'s type and `lowered` has that type.
        Ok(unsafe { LLVMBuildStore(self.codegen.builder, lowered, element_slot) })
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
        let llvm_type = self.codegen.llvm_type(ty)?;
        let pointer = self.local_pointer(slot)?;
        let name = c_string(&format!("local.{slot}.read"));
        // SAFETY: `pointer` is this slot's alloca of `llvm_type`.
        let value =
            unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, name.as_ptr()) };
        self.copy_value(value, ty)
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
            let lowered = self.lower_expr(field)?;
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
        if let Some(value) = self.field_of_borrowed_element(base, index, ty)? {
            return Ok(value);
        }
        let base_ty = self.type_of(base);
        let base_value = self.lower_expr(base)?;
        let field = self.extract_field(base_value, index)?;
        let copy = self.copy_value(field, ty)?;
        self.drop_value(base_value, base_ty)?;
        Ok(copy)
    }

    /// Reads one field of an array element without copying the element.
    ///
    /// `nodes[i].firstChild` asks for one scalar. Lowered as written it copies
    /// the whole element out first — every string, array and enum in it cloned
    /// and then dropped again — to read one word of it. A layout pass walks
    /// thousands of nodes per frame doing exactly this, so that copy *was* the
    /// frame.
    ///
    /// The element is addressable, so the field is too: walk to the element's
    /// slot, walk to the field inside it, and copy only what was asked for. The
    /// array is borrowed rather than cloned, on the same terms as
    /// [`Self::lower_index`] — this expression does not own it and does not free
    /// it.
    ///
    /// Returns `None` when the base is not an element of a borrowable array, and
    /// the general path handles it.
    fn field_of_borrowed_element(
        &mut self,
        base: IrExprId,
        index: u32,
        ty: Type,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        let IrExpr::Index {
            base: array,
            index: at,
            ty: element_ty,
        } = *self.codegen.program.expr(base)
        else {
            return Ok(None);
        };
        if !matches!(element_ty, Type::Struct(_)) {
            return Ok(None);
        }
        let Some(handle) = self.borrowed_local_handle(array)? else {
            return Ok(None);
        };
        let slot = self.element_slot(handle, at, element_ty)?;
        let struct_type = self.codegen.llvm_type(element_ty)?;
        let name = c_string(&format!("elem.field.{index}.ptr"));
        // SAFETY: `slot` addresses one live element of `struct_type`, bounds
        // checked by the runtime, and `index` came from that struct's own
        // definition.
        let field_ptr = unsafe {
            LLVMBuildStructGEP2(
                self.codegen.builder,
                struct_type,
                slot,
                index,
                name.as_ptr(),
            )
        };
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: the field pointer addresses a live value of `llvm_type`.
        let field = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                llvm_type,
                field_ptr,
                c"elem.field".as_ptr(),
            )
        };
        // The element still owns its field, so the reader gets a copy of that
        // one field — never of the element around it.
        Ok(Some(self.copy_value(field, ty)?))
    }

    /// Builds an enum value: a boxed tag plus its optional payload, encoded into
    /// the one type-erased word the runtime box carries.
    ///
    /// A scalar payload's bits go in directly; a `String` payload is an owned
    /// handle, so `owns_str` is set and the box takes ownership of it — exactly
    /// what makes the box's clone/free reclaim it. A payload-less variant passes
    /// a zero word and `owns_str` unset.
    fn lower_enum_new(
        &mut self,
        enum_id: kira_semantics_model::EnumId,
        tag: u32,
        payload: Option<IrExprId>,
    ) -> Result<LLVMValueRef, LlvmError> {
        let tag_value = self.codegen.const_int(i64::from(tag));
        let Some(payload) = payload else {
            // A variant with no payload is nothing but a tag, and the handle
            // holds it: `(tag << 1) | 1`, a constant, with no allocation and no
            // call. The runtime recognizes the low bit and treats clone as
            // identity and free as nothing. See `kira_native_bridge::enums`.
            return Ok(self.codegen.inline_enum(tag));
        };
        let payload_ty = self.codegen.enum_payload_type(enum_id, tag)?;
        let value = self.lower_expr(payload)?;
        if matches!(payload_ty, Type::Struct(_)) {
            return self.lower_enum_aggregate_new(tag_value, payload_ty, value);
        }
        let (kind, payload_word) = self.encode_enum_payload(payload_ty, value)?;
        Ok(self.call(
            self.codegen.runtime.enum_new,
            &mut [tag_value, kind, payload_word],
            c"enum",
        ))
    }

    /// Moves a struct payload into the runtime's erased aggregate box.
    fn lower_enum_aggregate_new(
        &mut self,
        tag: LLVMValueRef,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this context, `value` has that type, and
        // the builder is positioned on a live block.
        let slot = unsafe {
            let slot = LLVMBuildAlloca(
                self.codegen.builder,
                llvm_type,
                c"enum.aggregate.source".as_ptr(),
            );
            LLVMBuildStore(self.codegen.builder, value, slot);
            slot
        };
        let size = self.codegen.abi_size(ty)?;
        let clone = self.codegen.element_clone(ty)?;
        let free = self.codegen.element_free(ty)?;
        Ok(self.call(
            self.codegen.runtime.enum_new_aggregate,
            &mut [tag, slot, size, clone, free],
            c"enum.aggregate",
        ))
    }

    /// Encodes a payload value into `(payload_kind, payload_word)` for the enum
    /// box.
    fn encode_enum_payload(
        &mut self,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(LLVMValueRef, LLVMValueRef), LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: `value` has `ty`'s LLVM type and the builder is on a live
        // block; each conversion below targets `i64`, the box's payload word.
        let word = unsafe {
            match ty {
                Type::Int(_) => value,
                Type::Float(_) => {
                    LLVMBuildBitCast(builder, value, types.i64, c"enum.float.bits".as_ptr())
                }
                Type::Bool => LLVMBuildZExt(builder, value, types.i64, c"enum.bool.bits".as_ptr()),
                // A nested enum is a handle exactly as a `String` is, so it
                // encodes the same way; only the kind the box records differs,
                // which is what makes its clone/free recurse.
                Type::String | Type::Enum(_) => {
                    LLVMBuildPtrToInt(builder, value, types.i64, c"enum.handle.bits".as_ptr())
                }
                _ => {
                    return Err(LlvmError::Unsupported(
                        "an enum payload of an unsupported type",
                    ));
                }
            }
        };
        let kind = self.codegen.const_int(payload_kind(ty));
        Ok((kind, word))
    }

    /// Reads an enum value's discriminant tag as an `Int`.
    ///
    /// The VM's `EnumTag`, in the same order: the value is evaluated (a local
    /// read clones the enum), the tag is read out, and then the clone is freed —
    /// exactly as `.count` reads and frees an array.
    fn lower_enum_tag(&mut self, value: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let enum_value = self.lower_expr(value)?;
        let tag = self.call(
            self.codegen.runtime.enum_tag,
            &mut [enum_value],
            c"enum.tag",
        );
        self.drop_value(enum_value, value_ty)?;
        Ok(tag)
    }

    /// Reads an enum value's payload as an owned value of type `ty`.
    ///
    /// The same order as the VM's `EnumPayload`: the enum is evaluated (a local
    /// read clones it), the payload is read *owned* — `kira_rt_enum_payload`
    /// clones a `String` — and only then is the enum released. Reading before
    /// releasing is what keeps a `String` payload alive across the free.
    fn lower_enum_payload(&mut self, value: IrExprId, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let value_ty = self.type_of(value);
        let enum_value = self.lower_expr(value)?;
        let decoded = if matches!(ty, Type::Struct(_)) {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `llvm_type` belongs to this context and the runtime writes
            // one owned value of exactly that type into `out`.
            let out = unsafe {
                LLVMBuildAlloca(
                    self.codegen.builder,
                    llvm_type,
                    c"enum.aggregate.payload".as_ptr(),
                )
            };
            self.call(
                self.codegen.runtime.enum_payload_aggregate,
                &mut [enum_value, out],
                c"",
            );
            // SAFETY: the helper initialized `out` with a value of `llvm_type`.
            unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    llvm_type,
                    out,
                    c"enum.aggregate.value".as_ptr(),
                )
            }
        } else {
            let word = self.call(
                self.codegen.runtime.enum_payload,
                &mut [enum_value],
                c"enum.payload",
            );
            self.decode_enum_payload(ty, word)?
        };
        self.drop_value(enum_value, value_ty)?;
        Ok(decoded)
    }

    /// Decodes a payload word back into a value of type `ty`.
    ///
    /// The exact inverse of [`Self::encode_enum_payload`], which is what makes a
    /// round trip through the box lossless on every payload type the
    /// declaration admits.
    fn decode_enum_payload(
        &mut self,
        ty: Type,
        word: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: `word` is the `i64` the box stores for a payload of `ty`, put
        // there by `encode_enum_payload`, and the builder is on a live block.
        unsafe {
            Ok(match ty {
                Type::Int(_) => word,
                Type::Float(_) => {
                    LLVMBuildBitCast(builder, word, types.f64, c"enum.payload.float".as_ptr())
                }
                Type::Bool => {
                    LLVMBuildTrunc(builder, word, types.i1, c"enum.payload.bool".as_ptr())
                }
                Type::String | Type::Enum(_) => {
                    LLVMBuildIntToPtr(builder, word, types.ptr, c"enum.payload.handle".as_ptr())
                }
                _ => {
                    return Err(LlvmError::Unsupported(
                        "an enum payload of an unsupported type",
                    ));
                }
            })
        }
    }
}

/// The payload kind the enum box records for a payload of type `ty`.
///
/// Mirrors `kira_native_bridge::enums`' `PAYLOAD_*` constants, which decide what
/// the box's clone and free reclaim. The two are kept in step by
/// `the_payload_kinds_match_the_runtime`, below — the backend and the runtime
/// archive are compiled separately, so nothing but a test makes them agree.
fn payload_kind(ty: Type) -> i64 {
    match ty {
        Type::String => EnumPayloadKind::STR,
        Type::Enum(_) => EnumPayloadKind::ENUM,
        Type::Struct(_) => EnumPayloadKind::AGGREGATE,
        _ => EnumPayloadKind::INERT,
    }
    .as_i64()
}

#[cfg(test)]
mod tests {
    use super::payload_kind;
    use kira_runtime_abi::EnumPayloadKind;
    use kira_semantics_model::{EnumDef, EnumTable, StructDef, Type, TypeTable};

    /// The kinds this lowering emits are the ones the runtime interprets.
    ///
    /// A drift here is the silent failure the ABI marker exists to catch: the
    /// symbols still resolve, and the box simply forgets to free its payload.
    #[test]
    fn the_payload_kinds_match_the_runtime() {
        assert_eq!(payload_kind(Type::INT), EnumPayloadKind::INERT.as_i64());
        assert_eq!(payload_kind(Type::Bool), EnumPayloadKind::INERT.as_i64());
        assert_eq!(payload_kind(Type::String), EnumPayloadKind::STR.as_i64());
        // An id is minted only by the table, so the test declares one.
        let mut enums = EnumTable::new();
        let id = enums
            .declare(EnumDef {
                name: "E".to_owned(),
                variants: Vec::new(),
            })
            .expect("a fresh table accepts the first declaration");
        assert_eq!(payload_kind(Type::Enum(id)), EnumPayloadKind::ENUM.as_i64());

        let mut types = TypeTable::new();
        let id = types
            .structs_mut()
            .declare(StructDef {
                name: "Payload".to_owned(),
                fields: Vec::new(),
            })
            .expect("a fresh table accepts the first struct");
        assert_eq!(
            payload_kind(Type::Struct(id)),
            EnumPayloadKind::AGGREGATE.as_i64()
        );
    }
}
