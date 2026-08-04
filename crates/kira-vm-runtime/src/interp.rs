//! The bytecode interpreter: a match-in-loop stack machine.
//!
//! The interpreter keeps call frames on a heap-allocated stack (so Kira
//! recursion never consumes the host's native stack) and a single shared
//! operand stack. It touches the outside world only through the
//! [`HostCapabilities`] trait, so the whole crate stays portable to
//! `wasm32-unknown-unknown`.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{HostCapabilities, NativeStatePathStep, TaskExecutor};

use crate::error::{NativeStateOperation, VmError};
use crate::value::{Heap, Value};

mod cells;
mod compiler;
mod env;
mod file_system;
mod frames;
mod host;
mod native_state;
mod operators;
mod place;
mod program;
mod strings;

pub(crate) use self::program::check_signature;
pub use self::program::{Program, RunOutcome, execute};

use self::frames::{Frame, Writeback, new_frame};
use self::place::{ResolvedStep, check_index};

/// Guards against unbounded recursion turning into unbounded memory use.
const MAX_CALL_DEPTH: usize = 1 << 20;

/// The running interpreter: a host, a heap, an operand stack, and scratch.
pub(crate) struct Vm<'h> {
    host: &'h mut dyn HostCapabilities,
    pub(crate) heap: Heap,
    stack: Vec<Value>,
    /// Reusable scratch for a dynamic place's resolved steps.
    ///
    /// A `StorePlace`/`ArrayAppend` resolves its path into this buffer once per
    /// execution; keeping it on the VM and reusing its capacity is what keeps
    /// those ops off the per-op allocation the interpreter's hot loop forbids.
    /// It is taken out with `mem::take` while filled — so the fill can pop the
    /// operand stack without borrowing the VM twice — then handed back cleared,
    /// never freed.
    steps: Vec<ResolvedStep>,
    /// Reusable scratch for a path into callback state.
    ///
    /// Same reason as `steps`: a write through a recovered view happens on every
    /// mutation of native state, and allocating its path each time would put an
    /// allocation on that path.
    native_path: Vec<NativeStatePathStep>,
    /// The deferred tasks this run spawned.
    ///
    /// One table per run, because a handle is an index into it: two runs
    /// sharing a table would let one program's handle name another's task. The
    /// *policy* is not here — the scheduler is generated Kira the IR
    /// synthesizes, so this only answers what each primitive means.
    tasks: TaskExecutor,
}

impl Vm<'_> {
    /// Runs to completion, reclaiming everything still live if it traps.
    ///
    /// A trap leaves live frames and a non-empty operand stack, and both hold
    /// heap storage. Freeing them here is what makes heap accounting mean
    /// something after a failed call: when the heap belongs to one run it is
    /// about to be dropped anyway, but an [`crate::Instance`]'s heap outlives
    /// the call, so a trap that left its frames behind would leak into it.
    fn run(&mut self, module: &Module, entry: Frame) -> Result<Value, VmError> {
        let mut frames = vec![entry];
        match self.dispatch(module, &mut frames) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.unwind(&mut frames);
                Err(error)
            }
        }
    }

    /// Frees every local of every live frame and everything left on the operand
    /// stack.
    fn unwind(&mut self, frames: &mut Vec<Frame>) {
        for frame in frames.drain(..) {
            self.discard(frame.locals);
        }
        let leftovers = std::mem::take(&mut self.stack);
        self.discard(leftovers);
    }

    fn dispatch(&mut self, module: &Module, frames: &mut Vec<Frame>) -> Result<Value, VmError> {
        loop {
            let depth = frames.len() - 1;
            let frame = &mut frames[depth];
            let func = &module.functions[frame.func as usize];
            let instruction = func.code[frame.pc].clone();
            frame.pc += 1;

            match instruction {
                Instruction::Return | Instruction::ReturnVoid => {
                    let result = if matches!(instruction, Instruction::Return) {
                        self.pop()?
                    } else {
                        Value::Void
                    };
                    let Some(mut finished) = frames.pop() else {
                        return Err(VmError::FrameUnderflow);
                    };
                    // A written-through parameter moves into the caller's place
                    // before this frame's locals are dropped, so its value lands
                    // in the place rather than being freed with the frame. Every
                    // target names a distinct parameter, so taking each in turn
                    // leaves the rest intact.
                    let writebacks = std::mem::take(&mut finished.writebacks);
                    for writeback in writebacks {
                        let Some(value) = finished
                            .locals
                            .get_mut(writeback.param as usize)
                            .map(|slot| std::mem::replace(slot, Value::Void))
                        else {
                            continue;
                        };
                        if let Err(error) = self.write_back(frames, &writeback, value) {
                            self.discard(finished.locals);
                            self.heap.drop_value(result);
                            return Err(error);
                        }
                    }
                    for local in finished.locals {
                        self.heap.drop_value(local);
                    }
                    if frames.is_empty() {
                        return Ok(result);
                    }
                    self.stack.push(result);
                }
                Instruction::Call(index) => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = new_frame(module, index)?;
                    if let Err(error) = self.fill_params(module, index, &mut callee) {
                        // The callee is not on the frame stack yet, so the
                        // unwind cannot see it — the arguments already moved
                        // into its slots are freed here instead.
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    frames.push(callee);
                }
                Instruction::CallNative(id) => self.call_native(module, id)?,
                Instruction::CallForeign(id) => self.call_foreign(module, id)?,
                Instruction::CallMut { func, slot, path } => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = new_frame(module, func)?;
                    // The writeback place's indices sit on top of the operand
                    // stack, pushed after the arguments; resolve them first so
                    // the arguments are exposed for `fill_params`.
                    let mut steps = Vec::new();
                    if let Err(error) = self.fill_steps(&path, &mut steps) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    if let Err(error) = self.fill_params(module, func, &mut callee) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    callee.writebacks = vec![Writeback {
                        param: 0,
                        slot,
                        steps,
                    }];
                    frames.push(callee);
                }
                Instruction::CallWriteback { func, targets } => {
                    if frames.len() >= MAX_CALL_DEPTH {
                        return Err(VmError::CallDepthExceeded);
                    }
                    let mut callee = new_frame(module, func)?;
                    // Every target's place indices sit on top of the operand
                    // stack, pushed after the arguments and targets in order, so
                    // they are resolved back to front — the last target's
                    // indices are the ones on top.
                    let mut writebacks = Vec::with_capacity(targets.len());
                    let mut failure = None;
                    for target in targets.iter().rev() {
                        let mut steps = Vec::new();
                        if let Err(error) = self.fill_steps(&target.path, &mut steps) {
                            failure = Some(error);
                            break;
                        }
                        writebacks.push(Writeback {
                            param: target.param,
                            slot: target.slot,
                            steps,
                        });
                    }
                    if let Some(error) = failure {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    // Back to parameter order, so a return writes the targets in
                    // the order the call declared them.
                    writebacks.reverse();
                    if let Err(error) = self.fill_params(module, func, &mut callee) {
                        self.discard(callee.locals);
                        return Err(error);
                    }
                    callee.writebacks = writebacks;
                    frames.push(callee);
                }
                other => self.step(module, &mut frames[depth], other)?,
            }
        }
    }

    /// Executes one non-control-flow-frame instruction against `frame`.
    fn step(
        &mut self,
        module: &Module,
        frame: &mut Frame,
        instruction: Instruction,
    ) -> Result<(), VmError> {
        match instruction {
            Instruction::ConstInt(value) => self.stack.push(Value::Int(value)),
            Instruction::ConstFloat(value) => self.stack.push(Value::Float(value)),
            Instruction::ConstBool(value) => self.stack.push(Value::Bool(value)),
            Instruction::ConstVoid => self.stack.push(Value::Void),
            Instruction::ConstRawPtrNull => self.stack.push(Value::RawPtr(0)),
            Instruction::ForeignCallback(id) => self.foreign_callback(module, id)?,
            Instruction::ConstStr(index) => {
                let text = module.strings[index as usize].clone();
                let id = self.heap.alloc(text);
                self.stack.push(Value::Str(id));
            }
            Instruction::LoadLocal(slot) => {
                let value = frame.locals[slot as usize];
                let copy = self.heap.copy_value(value);
                self.stack.push(copy);
            }
            Instruction::StoreLocal(slot) => {
                let value = self.pop()?;
                // Storing into a local that holds a recovered view writes THROUGH it
                // into the callback state — except when the incoming value is itself
                // a view. Rebinding the local to another view (`state =
                // nativeRecover<T>(other)`, or a slot the compiler reuses for a
                // second view) replaces what the local names; writing the second
                // view into the first one's state is both meaningless and
                // unrepresentable, since a view has no boxed form.
                let rebinding_a_view = matches!(value, Value::NativeView { .. });
                if let Value::NativeView { token, type_id } = frame.locals[slot as usize]
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
                    let old = std::mem::replace(&mut frame.locals[slot as usize], value);
                    self.heap.drop_value(old);
                }
            }
            Instruction::Pop => {
                let value = self.pop()?;
                self.heap.drop_value(value);
            }
            Instruction::NativeState(type_word) => self.native_state_new(type_word)?,
            Instruction::NativeUserData => self.native_user_data()?,
            Instruction::NativeRecover(type_word) => self.native_recover(type_word)?,
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
                let first = self
                    .stack
                    .len()
                    .checked_sub(count as usize)
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
                            &[NativeStatePathStep::Field(index.into())],
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
                        NativeStatePathStep::Field(index.into()),
                        VmError::NoSuchField { index },
                    )?;
                    self.stack.push(field);
                    return Ok(());
                }
                let Value::Struct(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAStruct);
                };
                let Some(field) = self.heap.field(id, index) else {
                    // The struct was ours the moment it left the stack; a
                    // refused projection frees it rather than abandoning it in
                    // a heap that may outlive this call.
                    self.heap.drop_value(base);
                    return Err(VmError::NoSuchField { index });
                };
                // The field is copied out before the struct is dropped: the
                // struct owns its fields, so handing one out without copying
                // would hand out storage this drop is about to free.
                let copy = self.heap.copy_value(field);
                self.heap.drop_value(base);
                self.stack.push(copy);
            }
            Instruction::StoreField { slot, path } => {
                let value = self.pop()?;
                // Every step is a constant field index, so the walk reads the
                // path directly — no scratch buffer, no allocation.
                if let Err(error) = self.store_field(frame, slot, path.steps(), value) {
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
                    vm.fill_steps(&path, steps)?;
                    vm.store_place(frame, slot, steps, value)
                });
                if let Err(error) = stored {
                    self.heap.drop_value(value);
                    return Err(error);
                }
            }
            Instruction::ArrayAppend { slot, path } => {
                let value = self.pop()?;
                let appended = self.with_steps(|vm, steps| {
                    vm.fill_steps(&path, steps)?;
                    vm.append_through(frame, slot, steps, value)
                });
                if let Err(error) = appended {
                    self.heap.drop_value(value);
                    return Err(error);
                }
            }
            Instruction::NewArray(count) => {
                let first = self
                    .stack
                    .len()
                    .checked_sub(count as usize)
                    .ok_or(VmError::StackUnderflow)?;
                // The elements were pushed in written order, so splitting them
                // off preserves that order — and moves them, so nothing is
                // copied and nothing is left on the stack to double-free.
                let elements = self.stack.split_off(first);
                let id = self.heap.alloc_array(elements);
                self.stack.push(Value::Array(id));
            }
            Instruction::ArrayGet => {
                let index = self.pop_int()?;
                let base = self.pop()?;
                if let Value::NativeSnapshot(id) = base {
                    let Ok(index) = u64::try_from(index) else {
                        self.heap.free_snapshot(id);
                        return Err(VmError::NegativeIndex);
                    };
                    let element = self.read_snapshot_child(
                        id,
                        NativeStatePathStep::Index(index),
                        VmError::IndexOutOfBounds,
                    )?;
                    self.stack.push(element);
                    return Ok(());
                }
                let Value::Array(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAnArray);
                };
                let read = check_index(index, self.heap.array_len(id)).and_then(|index| {
                    self.heap
                        .element(id, index)
                        .ok_or(VmError::IndexOutOfBounds)
                });
                let element = match read {
                    Ok(element) => element,
                    Err(error) => {
                        // The array was ours; a failed read frees it.
                        self.heap.drop_value(base);
                        return Err(error);
                    }
                };
                // The element is copied out before the array is dropped: the
                // array owns its elements, so handing one out without copying
                // would hand out storage this drop is about to free.
                let copy = self.heap.copy_value(element);
                self.heap.drop_value(base);
                self.stack.push(copy);
            }
            Instruction::TaskOp(prim) => {
                // Popped in reverse: the compiler pushed the three operands
                // deepest-first, so the last pushed is the third.
                let third = self.pop_int()?;
                let second = self.pop_int()?;
                let first = self.pop_int()?;
                let answer = self.tasks.perform(prim, first, second, third)?;
                self.stack.push(Value::Int(answer));
            }
            Instruction::ArrayGetLocal(slot) => {
                let index = self.pop_int()?;
                // The local is *borrowed*: its handle is read without copying
                // the array, and nothing is dropped here because this
                // instruction does not own it. Only the element is copied out,
                // which is what keeps a handed-out element unshared.
                if let Value::NativeSnapshot(id) = frame.locals[slot as usize] {
                    // Borrowed here too: the read stays in the local, so this
                    // takes a hold of its own rather than consuming it.
                    let Value::NativeSnapshot(borrowed) =
                        self.heap.copy_value(Value::NativeSnapshot(id))
                    else {
                        return Err(VmError::NotAnArray);
                    };
                    let Ok(index) = u64::try_from(index) else {
                        self.heap.free_snapshot(borrowed);
                        return Err(VmError::NegativeIndex);
                    };
                    let element = self.read_snapshot_child(
                        borrowed,
                        NativeStatePathStep::Index(index),
                        VmError::IndexOutOfBounds,
                    )?;
                    self.stack.push(element);
                    return Ok(());
                }
                let Value::Array(id) = frame.locals[slot as usize] else {
                    return Err(VmError::NotAnArray);
                };
                let index = check_index(index, self.heap.array_len(id))?;
                let element = self
                    .heap
                    .element(id, index)
                    .ok_or(VmError::IndexOutOfBounds)?;
                let copy = self.heap.copy_value(element);
                self.stack.push(copy);
            }
            Instruction::ArrayLen => {
                let base = self.pop()?;
                if let Value::NativeSnapshot(id) = base {
                    let len = self.snapshot_array_len(id)?;
                    let counted = i64::try_from(len).map_err(|_| VmError::ArrayTooLong)?;
                    self.stack.push(Value::Int(counted));
                    return Ok(());
                }
                let Value::Array(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAnArray);
                };
                let counted = self
                    .heap
                    .array_len(id)
                    .ok_or(VmError::NotAnArray)
                    .and_then(|len| i64::try_from(len).map_err(|_| VmError::ArrayTooLong));
                // The array is freed on every path out, not just the one that
                // produced a count.
                self.heap.drop_value(base);
                self.stack.push(Value::Int(counted?));
            }
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
                // The arguments were pushed in source order, so they come off
                // reversed; collecting and reversing restores the order the
                // operation reads them in. The receiver sits under them.
                let mut arguments = Vec::with_capacity(op.argument_count());
                for _ in 0..op.argument_count() {
                    arguments.push(self.pop()?);
                }
                arguments.reverse();
                let base = self.pop()?;
                let produced = self.perform_string_op(op, base, &arguments)?;
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
                let payload = if has_payload { Some(self.pop()?) } else { None };
                let id = self.heap.alloc_enum(u32::from(tag), payload);
                self.stack.push(Value::Enum(id));
            }
            Instruction::Erase(type_id) => {
                // The value is taken over by the box, exactly as an enum
                // payload is: nothing is copied, and nothing is left behind to
                // be freed twice.
                let value = self.pop()?;
                let id = self.heap.alloc_erased(type_id, value);
                self.stack.push(Value::Erased(id));
            }
            Instruction::NewCell => self.new_cell()?,
            Instruction::CellGet(slot) => self.cell_get(frame, slot)?,
            Instruction::CellSet(slot) => self.cell_set(frame, slot)?,
            Instruction::EnumTag => {
                let base = self.pop()?;
                if let Value::NativeSnapshot(id) = base {
                    let tag = self.snapshot_enum_tag(id)?;
                    self.stack.push(Value::Int(i64::from(tag)));
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
                self.stack.push(Value::Int(i64::from(tag?)));
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
            Instruction::Jump(target) => self.jump(module, frame, target)?,
            Instruction::JumpIfFalse(target) => {
                let condition = self.pop_bool()?;
                if !condition {
                    self.jump(module, frame, target)?;
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
                let word = kira_runtime_abi::c_storage::retain_text(self.heap.get(id));
                self.heap.drop_value(value);
                self.stack.push(Value::RawPtr(word));
            }
            Instruction::CLayoutAddress(aggregate) => {
                let value = self.pop()?;
                let id = kira_runtime_abi::ForeignAggregateId(aggregate);
                let bytes = self.heap.aggregate_bytes(
                    &module.foreign_aggregates,
                    id,
                    value,
                    kira_runtime_abi::ForeignPointerWidth::HOST,
                );
                self.heap.drop_value(value);
                let bytes = bytes.map_err(|_| VmError::TypeMismatch {
                    expected: "a C-layout struct",
                })?;
                self.stack
                    .push(Value::RawPtr(kira_runtime_abi::c_storage::retain_bytes(
                        &bytes,
                    )));
            }
            Instruction::FileSystem(op) => self.file_system(op)?,
            Instruction::Compiler(op) => self.compiler(op)?,
            Instruction::Env(op) => self.env(op)?,
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
            arithmetic => self.binary(arithmetic)?,
        }
        Ok(())
    }

    fn jump(&self, module: &Module, frame: &mut Frame, target: u32) -> Result<(), VmError> {
        let len = module.functions[frame.func as usize].code.len() as u32;
        // A target must land on a real instruction; `len` (one past the end)
        // is out of range and would read past the code on the next step.
        if target >= len {
            return Err(VmError::BadJump(target));
        }
        frame.pc = target as usize;
        Ok(())
    }

    // ----- operand-stack helpers ---------------------------------------

    fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    /// Reports a mismatched operand, freeing it first.
    ///
    /// The typed pops below take the value off the stack before they know it is
    /// the wrong one, and a popped value is this VM's to own. Well-typed
    /// bytecode never reaches here, but a `Module` is a public artifact and
    /// validation proves structure rather than stack typing — so an ill-typed
    /// module must trap without stranding storage in a heap that may outlive
    /// the call.
    fn mismatch(&mut self, value: Value, expected: &'static str) -> VmError {
        self.heap.drop_value(value);
        VmError::TypeMismatch { expected }
    }

    fn pop_int(&mut self) -> Result<i64, VmError> {
        match self.pop()? {
            Value::Int(value) => Ok(value),
            other => Err(self.mismatch(other, "Int")),
        }
    }

    fn pop_float(&mut self) -> Result<f64, VmError> {
        match self.pop()? {
            Value::Float(value) => Ok(value),
            other => Err(self.mismatch(other, "Float")),
        }
    }

    fn pop_bool(&mut self) -> Result<bool, VmError> {
        match self.pop()? {
            Value::Bool(value) => Ok(value),
            other => Err(self.mismatch(other, "Bool")),
        }
    }

    fn pop_str(&mut self) -> Result<crate::value::StrId, VmError> {
        match self.pop()? {
            Value::Str(id) => Ok(id),
            other => Err(self.mismatch(other, "String")),
        }
    }

    /// Pops the two string operands of a binary string op, right one first.
    ///
    /// Paired here rather than at each call site because the second pop is the
    /// one that can fail with the first already in hand: an ill-typed module
    /// would otherwise strand the right operand in a local no unwind can see.
    fn pop_two_str(&mut self) -> Result<(crate::value::StrId, crate::value::StrId), VmError> {
        let rhs = self.pop_str()?;
        match self.pop_str() {
            Ok(lhs) => Ok((lhs, rhs)),
            Err(error) => {
                self.heap.free(rhs);
                Err(error)
            }
        }
    }
}
