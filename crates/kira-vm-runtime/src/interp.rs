//! The bytecode interpreter: a match-in-loop stack machine.
//!
//! The interpreter keeps call frames on a heap-allocated stack (so Kira
//! recursion never consumes the host's native stack) and a single shared
//! operand stack. It touches the outside world only through the
//! [`HostCapabilities`] trait, so the whole crate stays portable to
//! `wasm32-unknown-unknown`.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{HostCapabilities, NativeArg};

use crate::error::VmError;
use crate::value::{Heap, Value};

mod host;
mod native_state;
mod operators;
mod place;
mod program;

pub(crate) use self::program::check_signature;
pub use self::program::{Program, RunOutcome, execute};

use self::place::{ResolvedStep, check_index};

/// Guards against unbounded recursion turning into unbounded memory use.
const MAX_CALL_DEPTH: usize = 1 << 20;

/// One call frame: its function, program counter, and local slots.
struct Frame {
    func: u32,
    pc: usize,
    locals: Vec<Value>,
    /// Where this frame's final receiver (slot 0) is written back on return.
    ///
    /// `Some` only for a frame entered by [`Instruction::CallMut`] — a mutating
    /// method. On return the callee's mutated receiver moves into the caller's
    /// place before the callee's locals are dropped, which is the whole of
    /// value-semantics writeback. `None` for every ordinary call.
    writeback: Option<Writeback>,
}

/// A resolved receiver-writeback target on a callee frame.
struct Writeback {
    /// The caller-frame local slot the place is rooted at.
    slot: u16,
    /// The steps to walk into the caller's storage, indices already resolved;
    /// empty writes the caller's local slot itself.
    steps: Vec<ResolvedStep>,
}

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
}

impl<'h> Vm<'h> {
    /// A VM that runs on `heap` and reaches the world through `host`.
    ///
    /// The heap is taken rather than created here because it does not always
    /// belong to one run: [`crate::Instance`] lends the VM a heap that outlives
    /// the call and takes it back afterwards.
    pub(crate) fn new(host: &'h mut dyn HostCapabilities, heap: Heap) -> Self {
        Vm {
            host,
            heap,
            stack: Vec::new(),
            steps: Vec::new(),
        }
    }

    /// Gives the heap back, whatever the run did with it.
    pub(crate) fn into_heap(self) -> Heap {
        self.heap
    }

    /// Runs `function_id` with `args` in its parameter slots, to completion.
    ///
    /// Arguments are lowered into this run's own heap, so the caller's storage
    /// is only read: a `&str` argument is copied in rather than aliased.
    fn enter(
        &mut self,
        module: &Module,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<Value, VmError> {
        let mut lowered = Vec::with_capacity(args.len());
        for argument in args {
            match self.heap.lower(*argument) {
                Some(value) => lowered.push(value),
                None => {
                    // The arguments already lowered are this heap's; a refused
                    // call frees them rather than leaving them behind.
                    self.discard(lowered);
                    return Err(VmError::HandleAtSeam {
                        function: function_id,
                    });
                }
            }
        }
        self.enter_values(module, function_id, lowered)
    }

    /// Runs `function_id` with values already lowered into this VM's heap.
    ///
    /// Takes ownership of `args`: every one of them is either moved into a
    /// parameter slot — and dropped with the frame — or freed here, on every
    /// path out, including the ones that never start the function.
    pub(crate) fn enter_values(
        &mut self,
        module: &Module,
        function_id: u32,
        args: Vec<Value>,
    ) -> Result<Value, VmError> {
        let mut frame = match new_frame(module, function_id) {
            Ok(frame) => frame,
            Err(error) => {
                self.discard(args);
                return Err(error);
            }
        };
        if args.len() > frame.locals.len() {
            // Validation proves `param_count <= local_count` and the entry
            // points check arity, so this is unreachable through either door —
            // it is here so the impossible case frees rather than panics. The
            // count is read before the values are freed, so the refusal names
            // what actually arrived rather than a placeholder.
            let got = args.len();
            self.discard(args);
            self.discard(frame.locals);
            return Err(VmError::ArityMismatch {
                function: function_id,
                expected: module.functions[function_id as usize].param_count,
                got,
            });
        }
        for (slot, value) in args.into_iter().enumerate() {
            frame.locals[slot] = value;
        }
        self.run(module, frame)
    }

    /// Frees a batch of values this VM owns.
    fn discard(&mut self, values: impl IntoIterator<Item = Value>) {
        for value in values {
            self.heap.drop_value(value);
        }
    }

    /// Writes a returning mutating method's final receiver back into the
    /// caller's place — the whole of value-semantics writeback.
    ///
    /// The caller is the frame just beneath the one that returned, always
    /// present because a mutating method is only ever entered by
    /// [`Instruction::CallMut`] from a caller. An empty path overwrites the
    /// caller's local itself (`g.mutate()`); a non-empty one walks into it,
    /// exactly as [`Vm::store_place`] does. The value overwritten is dropped;
    /// `receiver` is moved into the place, so it is not double-freed with the
    /// callee's other locals. The `slot` was bounds-checked against the caller's
    /// function by [`Module::validate`], so the direct index matches the
    /// discipline every other place walk follows.
    fn write_back(
        &mut self,
        frames: &mut [Frame],
        writeback: &Writeback,
        receiver: Value,
    ) -> Result<(), VmError> {
        let Some(caller) = frames.last_mut() else {
            self.heap.drop_value(receiver);
            return Err(VmError::FrameUnderflow);
        };
        if writeback.steps.is_empty() {
            let previous = std::mem::replace(&mut caller.locals[writeback.slot as usize], receiver);
            self.heap.drop_value(previous);
            return Ok(());
        }
        match self.store_place(caller, writeback.slot, &writeback.steps, receiver) {
            Ok(()) => Ok(()),
            Err(error) => {
                // A malformed place — unreachable past validation and typing —
                // leaves the receiver unstored, so it is freed rather than
                // leaked into a heap whose accounting a trap still checks.
                self.heap.drop_value(receiver);
                Err(error)
            }
        }
    }

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
                    // A mutating method writes its final receiver — slot 0 —
                    // back into the caller's place before its own locals are
                    // dropped, so the receiver moves into the place rather than
                    // being freed with the frame.
                    if let Some(writeback) = finished.writeback.take()
                        && let Some(receiver) = finished
                            .locals
                            .first_mut()
                            .map(|slot| std::mem::replace(slot, Value::Void))
                        && let Err(error) = self.write_back(frames, &writeback, receiver)
                    {
                        self.discard(finished.locals);
                        self.heap.drop_value(result);
                        return Err(error);
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
                    callee.writeback = Some(Writeback { slot, steps });
                    frames.push(callee);
                }
                other => self.step(module, &mut frames[depth], other)?,
            }
        }
    }

    /// Pops arguments off the operand stack into a fresh callee frame's
    /// parameter slots (arguments were pushed left to right).
    ///
    /// Fills in place rather than taking the frame, so a mid-fill failure hands
    /// the caller back a frame holding the slots already written — which it must
    /// free, because a partially filled frame never reaches the frame stack the
    /// unwind walks.
    fn fill_params(
        &mut self,
        module: &Module,
        index: u32,
        frame: &mut Frame,
    ) -> Result<(), VmError> {
        let param_count = module.functions[index as usize].param_count as usize;
        for slot in (0..param_count).rev() {
            frame.locals[slot] = self.pop()?;
        }
        Ok(())
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
                if let Value::NativeView { token, type_id } = frame.locals[slot as usize] {
                    let stored = self
                        .heap
                        .into_native_state(value)
                        .ok_or(VmError::NativeStateValueMismatch)?;
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
                let mut base = self.pop()?;
                if let Value::NativeView { token, type_id } = base {
                    let stored = self
                        .host
                        .native_state_recover(token, type_id)
                        .map_err(VmError::NativeState)?;
                    base = self.heap.from_native_state(stored);
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
            Instruction::ArrayLen => {
                let base = self.pop()?;
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
            Instruction::NewEnum { tag, has_payload } => {
                // The payload, when present, was pushed last, so it comes off
                // first and the box takes ownership of it — nothing is copied
                // and nothing is left on the stack to double-free.
                let payload = if has_payload { Some(self.pop()?) } else { None };
                let id = self.heap.alloc_enum(u32::from(tag), payload);
                self.stack.push(Value::Enum(id));
            }
            Instruction::EnumTag => {
                let base = self.pop()?;
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

fn new_frame(module: &Module, index: u32) -> Result<Frame, VmError> {
    let function = module
        .functions
        .get(index as usize)
        .ok_or(VmError::UnknownFunction(index))?;
    Ok(Frame {
        func: index,
        pc: 0,
        locals: vec![Value::Void; function.local_count as usize],
        writeback: None,
    })
}
