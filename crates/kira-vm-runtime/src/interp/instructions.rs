//! Execution of non-control-flow bytecode instructions.
//!
//! The dispatch loop lives in the parent module; this file keeps the large
//! instruction semantics match separate so the loop and its ownership helpers
//! remain reviewable.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::NativeStatePathStep;

use super::frames::Frame;
use super::{Vm, foreign_scalar_value};
use crate::error::{NativeStateOperation, VmError};
use crate::value::Value;

impl Vm<'_> {
    /// Pushes a local's value onto the operand stack, copying only when the
    /// value actually owns heap storage.
    #[inline(always)]
    pub(super) fn load_local(&mut self, frame: &Frame, slot: u64) -> Result<(), VmError> {
        let slot = usize::try_from(slot).map_err(|_| VmError::LocalSlotOutOfRange(slot))?;
        let value = *frame
            .locals
            .get(slot)
            .ok_or(VmError::LocalSlotOutOfRange(slot as u64))?;
        // Scalars and opaque seam words are already independent values. Avoid
        // entering the heap copier for the overwhelmingly common loop-local
        // case; heap-backed values still take the deep/value-semantic copy
        // path.
        match value {
            Value::Int(_)
            | Value::Float(_)
            | Value::Bool(_)
            | Value::RawPtr(_)
            | Value::NativeState(_)
            | Value::NativeView { .. }
            | Value::Void => self.stack.push(value),
            _ => self.stack.push(self.heap.copy_value(value)),
        }
        Ok(())
    }

    /// Stores an operand into a local, including the special write-through
    /// behavior of a recovered native-state view.
    #[inline(always)]
    pub(super) fn store_local(&mut self, frame: &mut Frame, slot: u64) -> Result<(), VmError> {
        let slot = usize::try_from(slot).map_err(|_| VmError::LocalSlotOutOfRange(slot))?;
        let value = self.pop()?;
        let Some(current) = frame.locals.get(slot).copied() else {
            self.heap.drop_value(value);
            return Err(VmError::LocalSlotOutOfRange(slot as u64));
        };
        // Storing into a local that holds a recovered view writes THROUGH it
        // into the callback state — except when the incoming value is itself a
        // view. Rebinding a view replaces what the local names.
        let rebinding_a_view = matches!(value, Value::NativeView { .. });
        if let Value::NativeView { token, type_id } = current
            && !rebinding_a_view
        {
            let stored = self.heap.into_native_state(value).map_err(|kind| {
                VmError::NativeStateValueMismatch {
                    operation: NativeStateOperation::Store,
                    kind,
                }
            })?;
            self.host
                .native_state_replace(token, type_id, stored)
                .map_err(VmError::NativeState)?;
        } else {
            let old = std::mem::replace(
                frame
                    .locals
                    .get_mut(slot)
                    .ok_or(VmError::LocalSlotOutOfRange(slot as u64))?,
                value,
            );
            // Loop-shaped assignment constantly replaces scalar locals. Those
            // values own no heap storage, so skip the general recursive drop
            // walk and retain it for heap-backed values and snapshots.
            if matches!(
                old,
                Value::Str(_)
                    | Value::Struct(_)
                    | Value::Array(_)
                    | Value::Enum(_)
                    | Value::Erased(_)
                    | Value::Cell(_)
                    | Value::NativeSnapshot(_)
            ) {
                self.heap.drop_value(old);
            }
        }
        Ok(())
    }

    pub(super) fn step(
        &mut self,
        module: &Module,
        frame: &mut Frame,
        instruction: &Instruction,
    ) -> Result<(), VmError> {
        // A callback-state tree can let go of a capture cell anywhere — a store
        // write unsharing a level, a snapshot going away — and none of those
        // places holds this heap. Each is recorded and released here, so a cell
        // outlives the tree's last share by at most one instruction.
        self.heap.drain_released_cells();
        match instruction {
            Instruction::ConstInt(value) => self.stack.push(Value::Int(*value)),
            Instruction::ConstFloat(value) => self.stack.push(Value::Float(*value)),
            Instruction::ConstBool(value) => self.stack.push(Value::Bool(*value)),
            Instruction::ConstVoid => self.stack.push(Value::Void),
            Instruction::ConstRawPtrNull => self.stack.push(Value::RawPtr(0)),
            Instruction::ForeignCallback(id) => self.foreign_callback(module, *id)?,
            Instruction::ConstStr(index) => {
                let text = module
                    .strings
                    .get(
                        usize::try_from(*index)
                            .map_err(|_| VmError::StringConstantOutOfRange(*index))?,
                    )
                    .ok_or(VmError::StringConstantOutOfRange(*index))?
                    .clone();
                let id = self.heap.alloc(text);
                self.stack.push(Value::Str(id));
            }
            Instruction::LoadLocal(slot) => self.load_local(frame, *slot)?,
            Instruction::StoreLocal(slot) => self.store_local(frame, *slot)?,
            Instruction::Pop => {
                let value = self.pop()?;
                self.heap.drop_value(value);
            }
            Instruction::NativeState(type_word) => self.native_state_new(*type_word)?,
            Instruction::NativeUserData => self.native_user_data()?,
            Instruction::NativeRecover(type_word) => self.native_recover(*type_word)?,
            Instruction::NativeStateFree => self.native_state_free()?,
            Instruction::Print => {
                let value = self.pop()?;
                let line = self
                    .heap
                    .format_and_consume(value)
                    .ok_or(VmError::UnprintableValue)?;
                self.host.write_line(&line);
                self.stack.push(Value::Void);
            }
            Instruction::NewStruct(count) => {
                let count = usize::try_from(*count).map_err(|_| VmError::ArrayTooLong)?;
                let first = self
                    .stack
                    .len()
                    .checked_sub(count)
                    .ok_or(VmError::StackUnderflow)?;
                // The fields were pushed in declaration order, so splitting
                // them off preserves layout order — and moves them, so nothing
                // is copied and nothing is left on the stack to double-free.
                let fields = self.stack.split_off(first);
                let id = self.heap.alloc_struct(fields);
                self.stack.push(Value::Struct(id));
            }
            Instruction::GetField(index) => {
                let base = self.pop()?;
                if let Value::NativeView { token, type_id } = base {
                    // Read the one field, not the whole state. Recovering the
                    // state here rebuilt every string, array and struct it holds
                    // as heap objects — so reading a counter out of a UI batch
                    // rebuilt its glyph cache, and reading three fields rebuilt
                    // it three times.
                    let stored = self
                        .host
                        .native_state_read(
                            token,
                            type_id,
                            &[NativeStatePathStep::Field(
                                u32::try_from(*index)
                                    .map_err(|_| VmError::NoSuchField { index: *index })?,
                            )],
                        )
                        .map_err(VmError::NativeState)?;
                    // An aggregate stops here as the node it already is. A whole
                    // widget tree read out of state is one refcount, and the
                    // reads that walk into it are refcounts too.
                    let value = self.heap.read_state_node(stored);
                    self.stack.push(value);
                    return Ok(());
                }
                if let Value::NativeSnapshot(id) = base {
                    let field = self.read_snapshot_child(
                        id,
                        NativeStatePathStep::Field(
                            u32::try_from(*index)
                                .map_err(|_| VmError::NoSuchField { index: *index })?,
                        ),
                        VmError::NoSuchField { index: *index },
                    )?;
                    self.stack.push(field);
                    return Ok(());
                }
                let Value::Struct(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAStruct);
                };
                let Some(field) = self.heap.field(id, *index) else {
                    // The struct was ours the moment it left the stack; a
                    // refused projection frees it rather than abandoning it in
                    // a heap that may outlive this call.
                    self.heap.drop_value(base);
                    return Err(VmError::NoSuchField { index: *index });
                };
                // The field is copied out before the struct is dropped: the
                // struct owns its fields, so handing one out without copying
                // would hand out storage this drop is about to free.
                //
                // A C-block field is the exception: a member read *resolves* to
                // the pointer word C would see rather than cloning the block,
                // because the block belongs to the struct and only the struct.
                // The word is exactly as raw as the same read on native — it
                // outlives nothing; the struct's lifetime is what keeps it
                // good.
                let copy = match field {
                    Value::CBlock(_) => self.heap.seam_word(field),
                    _ => self.heap.copy_value(field),
                };
                self.heap.drop_value(base);
                self.stack.push(copy);
            }
            Instruction::StoreField { slot, path } => {
                let value = self.pop()?;
                // Every step is a constant field index, so the walk reads the
                // path directly — no scratch buffer, no allocation.
                if let Err(error) = self.store_field(frame, *slot, path.steps(), value) {
                    // The value was ours the moment it left the stack, so a
                    // failed write frees it rather than leaking it.
                    self.heap.drop_value(value);
                    return Err(error);
                }
            }
            Instruction::StorePlace { slot, path } => {
                // The value was pushed last, so it comes off first; the indices
                // are underneath it.
                let value = self.pop()?;
                let stored = self.with_steps(|vm, steps| {
                    vm.fill_steps(path, steps)?;
                    vm.store_place(frame, *slot, steps, value)
                });
                if let Err(error) = stored {
                    self.heap.drop_value(value);
                    return Err(error);
                }
            }
            Instruction::ArrayAppend { slot, path } => self.array_append(frame, *slot, path)?,
            Instruction::NewArray(count) => self.new_array(*count)?,
            Instruction::ArrayGet => self.array_get()?,
            Instruction::TaskOp(prim) => {
                // Popped in reverse: the compiler pushed the three operands
                // deepest-first, so the last pushed is the third.
                let third = self.pop_int()?;
                let second = self.pop_int()?;
                let first = self.pop_int()?;
                let answer = self.tasks.perform(*prim, first, second, third)?;
                self.stack.push(Value::Int(answer));
            }
            Instruction::ArrayGetLocal(slot) => self.array_get_local(frame, *slot)?,
            Instruction::ArrayLen => self.array_len()?,
            Instruction::StringLen => {
                let base = self.pop()?;
                let Value::Str(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAString);
                };
                // Bytes, not characters — the same units `charAt` and
                // `substring` index, and the same count the native helper
                // produces, which is what keeps the two engines agreeing on
                // text that is not all ASCII.
                let counted =
                    i64::try_from(self.heap.get(id).len()).map_err(|_| VmError::ArrayTooLong);
                // The string is freed on every path out, not just the one that
                // produced a count.
                self.heap.drop_value(base);
                self.stack.push(Value::Int(counted?));
            }
            Instruction::StringCharAt => {
                let index = self.pop()?;
                let base = self.pop()?;
                let read = self.read_char_at(base, index);
                self.stack.push(Value::Int(read?));
            }
            Instruction::StringSubstring => {
                let end = self.pop()?;
                let start = self.pop()?;
                let base = self.pop()?;
                let carved = self.carve_substring(base, start, end);
                let text = carved?;
                let id = self.heap.alloc(text);
                self.stack.push(Value::Str(id));
            }
            Instruction::StringIndexOf => {
                let needle = self.pop()?;
                let base = self.pop()?;
                let found = self.find_index_of(base, needle);
                self.stack.push(Value::Int(found?));
            }
            Instruction::StringOp(op) => {
                let produced = self.with_string_args(|vm, arguments| {
                    // The arguments were pushed in source order, so they come
                    // off reversed; collecting and reversing restores the
                    // order the operation reads them in. The receiver sits
                    // under them.
                    for _ in 0..op.argument_count() {
                        match vm.pop() {
                            Ok(value) => arguments.push(value),
                            Err(error) => {
                                vm.discard(std::mem::take(arguments));
                                return Err(error);
                            }
                        }
                    }
                    arguments.reverse();
                    let base = match vm.pop() {
                        Ok(base) => base,
                        Err(error) => {
                            vm.discard(std::mem::take(arguments));
                            return Err(error);
                        }
                    };
                    // `perform_string_op` drops every argument on both its
                    // success and error paths. Clear the handles before the
                    // scratch vector is returned to the VM.
                    let result = vm.perform_string_op(*op, base, arguments);
                    arguments.clear();
                    result
                })?;
                self.stack.push(produced);
            }
            Instruction::StringOf => {
                let value = self.pop()?;
                let text = self
                    .heap
                    .format_and_consume(value)
                    .ok_or(VmError::UnprintableValue)?;
                let id = self.heap.alloc(text);
                self.stack.push(Value::Str(id));
            }
            Instruction::NewEnum { tag, has_payload } => {
                // The payload, when present, was pushed last, so it comes off
                // first and the box takes ownership of it — nothing is copied
                // and nothing is left on the stack to double-free.
                let payload = if *has_payload {
                    Some(self.pop()?)
                } else {
                    None
                };
                let id = self.heap.alloc_enum(*tag, payload);
                self.stack.push(Value::Enum(id));
            }
            Instruction::Erase(type_id) => {
                // The value is taken over by the box, exactly as an enum
                // payload is: nothing is copied, and nothing is left behind to
                // be freed twice.
                let value = self.pop()?;
                let id = self.heap.alloc_erased(*type_id, value);
                self.stack.push(Value::Erased(id));
            }
            Instruction::NewCell => self.new_cell()?,
            Instruction::CellGet(slot) => self.cell_get(frame, *slot)?,
            Instruction::CellSet(slot) => self.cell_set(frame, *slot)?,
            Instruction::EnumTag => {
                let base = self.pop()?;
                if let Value::NativeSnapshot(id) = base {
                    let tag = self.snapshot_enum_tag(id)?;
                    self.stack.push(Value::Int(
                        i64::try_from(tag).map_err(|_| VmError::ArrayTooLong)?,
                    ));
                    return Ok(());
                }
                let Value::Enum(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAnEnum);
                };
                let tag = self.heap.enum_tag(id).ok_or(VmError::NotAnEnum);
                // The box is freed on every path out, not just the one that
                // found a tag.
                self.heap.drop_value(base);
                self.stack.push(Value::Int(
                    i64::try_from(tag?).map_err(|_| VmError::ArrayTooLong)?,
                ));
            }
            Instruction::EnumPayload => {
                // The same shape as `EnumTag`: the enum is consumed, an owned
                // copy of what was read is pushed, and the box is freed — so
                // the binding outlives the enum it came from.
                let base = self.pop()?;
                if let Value::NativeSnapshot(id) = base {
                    let payload = self.snapshot_enum_payload(id)?;
                    self.stack.push(payload);
                    return Ok(());
                }
                let Value::Enum(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAnEnum);
                };
                let Some(payload) = self.heap.enum_payload(id) else {
                    // Same rule as `GetField`: a refused projection frees the
                    // box it popped.
                    self.heap.drop_value(base);
                    return Err(VmError::MissingEnumPayload);
                };
                self.heap.drop_value(base);
                self.stack.push(payload);
            }
            Instruction::Jump(target) => self.jump(module, frame, *target)?,
            Instruction::JumpIfFalse(target) => {
                let condition = self.pop_bool()?;
                if !condition {
                    self.jump(module, frame, *target)?;
                }
            }
            Instruction::Not => {
                let value = self.pop_bool()?;
                self.stack.push(Value::Bool(!value));
            }
            Instruction::BitNot => {
                let value = self.pop_int()?;
                self.stack.push(Value::Int(!value));
            }
            Instruction::NegInt => {
                let value = self.pop_int()?;
                self.stack.push(Value::Int(value.wrapping_neg()));
            }
            Instruction::NegFloat => {
                let value = self.pop_float()?;
                self.stack.push(Value::Float(-value));
            }
            Instruction::ConvertIntToFloat => {
                // Signed `i64` to `f64`, round to nearest ties even — Rust's
                // `as` matches the native `sitofp`.
                let value = self.pop_int()?;
                self.stack.push(Value::Float(value as f64));
            }
            Instruction::ConvertIntToRawPtr => {
                let value = self.pop_int()?;
                self.stack.push(Value::RawPtr(value as u64));
            }
            Instruction::ConvertRawPtrToInt => {
                let value = self.pop_foreign_pointer()?;
                self.stack.push(Value::Int(value as i64));
            }
            Instruction::ConvertFloatToBits => {
                // A reinterpretation: the IEEE-754 bit pattern, unchanged. The
                // native backend bitcasts, which is the same 64 bits.
                let value = self.pop_float()?;
                self.stack.push(Value::Int(value.to_bits() as i64));
            }
            Instruction::CStringNew => {
                let value = self.pop()?;
                let Value::Str(id) = value else {
                    self.heap.drop_value(value);
                    return Err(VmError::NotAString);
                };
                // An owned block, not process-lifetime storage: it lives as
                // long as the value it lands in and is freed with it. An
                // interior NUL crosses as C's null, the same refusal the
                // transient argument path makes.
                let block = self.heap.get(id).to_owned();
                let block = self.heap.cblock_text(&block);
                self.heap.drop_value(value);
                self.stack.push(match block {
                    Some(block) => Value::CBlock(block),
                    None => Value::RawPtr(0),
                });
            }
            Instruction::ArrayElements(element) => {
                self.array_elements(*element)?;
            }
            Instruction::ScalarText => {
                let value = self.pop()?;
                let Value::Int(code) = value else {
                    self.heap.drop_value(value);
                    return Err(VmError::TypeMismatch {
                        expected: "a code point to render as text",
                    });
                };
                // A code point outside Unicode, or a surrogate half, has no
                // scalar and so no text; the empty string is what it renders
                // as rather than a trap, matching what the native runtime does.
                let text = u32::try_from(code)
                    .ok()
                    .and_then(char::from_u32)
                    .map(String::from)
                    .unwrap_or_default();
                let id = self.heap.alloc(text);
                self.stack.push(Value::Str(id));
            }
            Instruction::MathOp(op) => {
                // Popped back to front, because the operands were pushed in
                // source order: `pow(x, y)` finds `y` on top.
                let mut operands = [0.0f64; kira_runtime_abi::MathOp::MAX_ARGUMENTS];
                let count = op.argument_count();
                for slot in operands[..count].iter_mut().rev() {
                    let value = self.pop()?;
                    let Value::Float(value) = value else {
                        self.heap.drop_value(value);
                        return Err(VmError::TypeMismatch {
                            expected: "a float to take a maths operation of",
                        });
                    };
                    *slot = value;
                }
                self.stack.push(Value::Float(op.apply(&operands[..count])));
            }
            Instruction::ForeignOffset(offset) => {
                let address = self.pop_foreign_pointer()?;
                self.stack
                    .push(Value::RawPtr(address.wrapping_add(u64::from(*offset))));
            }
            Instruction::ForeignIndex(stride) => {
                let index = self.pop()?;
                let Value::Int(index) = index else {
                    self.heap.drop_value(index);
                    return Err(VmError::TypeMismatch {
                        expected: "an integer index into C storage",
                    });
                };
                let address = self.pop_foreign_pointer()?;
                let step = (index as u64).wrapping_mul(u64::from(*stride));
                self.stack.push(Value::RawPtr(address.wrapping_add(step)));
            }
            Instruction::ForeignLoad { offset, ty } => {
                let address = self.pop_foreign_pointer()?;
                let size = kira_runtime_abi::scalar_layout(
                    *ty,
                    kira_runtime_abi::ForeignPointerWidth::HOST,
                )
                .size;
                // SAFETY: the pointer came from the foreign seam, Kira has no
                // arithmetic to alter one, and the offset and size are the
                // target's own C layout. A null base is the one case a program
                // can produce and is refused rather than read.
                let Some(word) =
                    (unsafe { kira_runtime_abi::c_storage::read_bytes(address, *offset, size) })
                else {
                    return Err(VmError::NullForeignRead);
                };
                self.stack.push(foreign_scalar_value(*ty, word));
            }
            Instruction::CLayoutAddress(aggregate) => {
                let value = self.pop()?;
                let id = kira_runtime_abi::ForeignAggregateId(*aggregate);
                let bytes = self.heap.aggregate_bytes(
                    &module.foreign_aggregates,
                    id,
                    value,
                    kira_runtime_abi::ForeignPointerWidth::HOST,
                );
                let bytes = match bytes {
                    Ok(bytes) => bytes,
                    Err(_) => {
                        self.heap.drop_value(value);
                        return Err(VmError::TypeMismatch {
                            expected: "a C-layout struct",
                        });
                    }
                };
                // The image becomes the unique parent of every block whose
                // address its pointer members contain. Moving those children
                // keeps the graph valid across copies and engine boundaries.
                let block = self
                    .heap
                    .cblock_aggregate_image(
                        &module.foreign_aggregates,
                        id,
                        value,
                        kira_runtime_abi::ForeignPointerWidth::HOST,
                        bytes,
                    )
                    .map_err(|_| VmError::TypeMismatch {
                        expected: "a C-layout struct",
                    })?;
                self.stack.push(Value::CBlock(block));
            }
            Instruction::FileSystem(op) => self.file_system(*op)?,
            Instruction::Compiler(op) => self.compiler(*op)?,
            Instruction::Env(op) => self.env(*op)?,
            Instruction::ConvertBitsToFloat => {
                let value = self.pop_int()?;
                self.stack.push(Value::Float(f64::from_bits(value as u64)));
            }
            Instruction::ConvertBits32ToFloat => {
                let value = self.pop_int()?;
                // Only the low 32 bits are the pattern; widening happens after
                // the reinterpretation, because the same bits denote a
                // different number at the two widths.
                let bits = u32::try_from(value as u64 & u64::from(u32::MAX)).unwrap_or(0);
                self.stack
                    .push(Value::Float(f64::from(f32::from_bits(bits))));
            }
            Instruction::ConvertFloatToBits32 => {
                let value = self.pop_float()?;
                // Narrow first, then take the pattern: the rounding is part of
                // the answer, not an accident of the cast. `as f32` is round to
                // nearest even, the IEEE-754 default the native backend's
                // `fptrunc` also uses.
                self.stack
                    .push(Value::Int(i64::from((value as f32).to_bits())));
            }
            Instruction::ConvertFloatToInt => {
                // Truncate toward zero, saturating out-of-range to
                // `i64::MIN`/`i64::MAX` and mapping NaN to zero. Rust's saturating
                // `f64 as i64` is exactly this, and the native backend mirrors it
                // with a saturating select chain.
                let value = self.pop_float()?;
                self.stack.push(Value::Int(value as i64));
            }
            // The outer dispatch loop keeps scalar integer arithmetic and
            // comparisons out of this general matcher. These arms remain for
            // callers that enter `step` directly, while the normal interpreter
            // path handles them before it reaches this function. The general
            // dispatcher still handles every other arithmetic and comparison
            // instruction, including malformed bytecode's checked error path.
            Instruction::AddInt => self.add_int()?,
            Instruction::SubInt => self.sub_int()?,
            Instruction::MulInt => self.mul_int()?,
            Instruction::EqInt => self.eq_int()?,
            Instruction::NeInt => self.ne_int()?,
            Instruction::LtInt => self.lt_int()?,
            Instruction::LeInt => self.le_int()?,
            Instruction::GtInt => self.gt_int()?,
            Instruction::GeInt => self.ge_int()?,
            arithmetic => self.binary(arithmetic)?,
        }
        Ok(())
    }
}
