//! The bytecode interpreter: a match-in-loop stack machine.
//!
//! The interpreter keeps call frames on a heap-allocated stack (so Kira
//! recursion never consumes the host's native stack) and a single shared
//! operand stack. It touches the outside world only through the
//! [`HostCapabilities`] trait, so the whole crate stays portable to
//! `wasm32-unknown-unknown`.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{HostCapabilities, NativeArg, NativeResult};

use crate::error::VmError;
use crate::value::{Heap, HeapStats, Value};

mod operators;
mod place;

use self::place::{ResolvedStep, check_index};

/// Guards against unbounded recursion turning into unbounded memory use.
const MAX_CALL_DEPTH: usize = 1 << 20;

/// The outcome of a completed run.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RunOutcome {
    /// The value `@Main` returned (`Void` for a `Void` main).
    pub result: Value,
    /// Heap accounting at exit; `current` is 0 for a clean run.
    pub heap: HeapStats,
}

/// One call frame: its function, program counter, and local slots.
struct Frame {
    func: u32,
    pc: usize,
    locals: Vec<Value>,
}

/// Runs `module`'s entrypoint, sending output to `host`.
///
/// Returns the entrypoint's result and heap accounting on success, or a
/// [`VmError`] trap. The final result value is dropped before accounting, so a
/// clean run reports `current == 0`.
pub fn execute(module: &Module, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
    module.validate()?;
    run_entry(module, host)
}

/// Runs `module`'s entrypoint on a fresh VM, assuming it is already validated.
fn run_entry(module: &Module, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
    let mut vm = Vm {
        host,
        heap: Heap::new(),
        stack: Vec::new(),
        steps: Vec::new(),
    };
    let main = module.main.ok_or(VmError::NoEntrypoint)?;
    let result = vm.enter(module, main, &[])?;
    // The program's result is no longer referenced by anything; drop it so
    // heap accounting reflects a fully reclaimed program.
    vm.heap.drop_value(result);
    Ok(RunOutcome {
        result,
        heap: vm.heap.stats(),
    })
}

/// An owned [`Module`] proven safe to interpret.
///
/// A `Module` is a public, deserializable artifact, so every index and operand
/// in it is validated before anything is trusted — that is what lets
/// interpretation index without the bounds checks it would otherwise need, and
/// without panicking on a malformed artifact.
///
/// Validation is a whole-module pass, so it is done once here rather than per
/// entry. That matters for a hybrid program, where the native half calls back
/// into the VM through [`Program::call`] at every crossing: re-proving the
/// module on each call would make a boundary crossing cost a scan of the
/// program.
///
/// The module is *owned* rather than borrowed: a host loads bytecode from
/// somewhere (a `.kbc` file, a network, memory), and the thing that runs it is
/// the natural owner of it.
pub struct Program {
    module: Module,
}

impl Program {
    /// Validates `module` and takes ownership of it, or reports why it cannot
    /// be run.
    pub fn load(module: Module) -> Result<Program, VmError> {
        module.validate()?;
        Ok(Program { module })
    }

    /// The module being run.
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// Runs the entrypoint, sending output to `host`.
    pub fn run(&self, host: &mut dyn HostCapabilities) -> Result<RunOutcome, VmError> {
        run_entry(&self.module, host)
    }

    /// Runs one function by id with `args`, and returns what it produced.
    ///
    /// This is the mirror of [`HostCapabilities::call_native`]: that is how a
    /// running program reaches the native half, and this is how the native half
    /// reaches back. Both speak the same seam vocabulary, so an embedder hosting
    /// a hybrid program marshals one way in each direction and nothing else.
    ///
    /// Ownership follows the same rule in both directions: **args borrow** (a
    /// string arrives as a `&str` the caller still owns, and is copied into this
    /// run's heap) and **the result owns** (a returned string is handed out as
    /// an owned `String`, because handing a value out is a move).
    ///
    /// Each call runs on its own heap and operand stack. Nothing outlives the
    /// call — the result is copied out before the heap is dropped — so calls
    /// nest freely, which is exactly what a native function calling a
    /// `@Runtime` function that calls a `@Native` function needs.
    pub fn call(
        &self,
        host: &mut dyn HostCapabilities,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, VmError> {
        let function = self
            .module
            .functions
            .get(function_id as usize)
            .ok_or(VmError::UnknownFunction(function_id))?;
        if args.len() != usize::from(function.param_count) {
            return Err(VmError::ArityMismatch {
                function: function_id,
                expected: function.param_count,
                got: args.len(),
            });
        }

        let mut vm = Vm {
            host,
            heap: Heap::new(),
            stack: Vec::new(),
            steps: Vec::new(),
        };
        let result = vm.enter(&self.module, function_id, args)?;
        let lifted = vm.heap.lift(result);
        vm.heap.drop_value(result);
        lifted.ok_or(VmError::StructAtSeam {
            function: function_id,
        })
    }
}

struct Vm<'h> {
    host: &'h mut dyn HostCapabilities,
    heap: Heap,
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

impl Vm<'_> {
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
        let mut frame = new_frame(module, function_id)?;
        for (slot, argument) in args.iter().enumerate() {
            frame.locals[slot] = self.heap.lower(*argument).ok_or(VmError::HandleAtSeam {
                function: function_id,
            })?;
        }
        self.run(module, frame)
    }

    fn run(&mut self, module: &Module, entry: Frame) -> Result<Value, VmError> {
        let mut frames = vec![entry];

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
                    let Some(finished) = frames.pop() else {
                        return Err(VmError::FrameUnderflow);
                    };
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
                    let callee = new_frame(module, index)?;
                    let filled = self.fill_params(module, index, callee)?;
                    frames.push(filled);
                }
                Instruction::CallNative(id) => self.call_native(module, id)?,
                other => self.step(module, &mut frames[depth], other)?,
            }
        }
    }

    /// Pops arguments off the operand stack into a fresh callee frame's
    /// parameter slots (arguments were pushed left to right).
    fn fill_params(
        &mut self,
        module: &Module,
        index: u32,
        mut frame: Frame,
    ) -> Result<Frame, VmError> {
        let param_count = module.functions[index as usize].param_count as usize;
        for slot in (0..param_count).rev() {
            frame.locals[slot] = self.pop()?;
        }
        Ok(frame)
    }

    /// Calls into the native half through the embedder.
    ///
    /// The VM performs no part of the call itself: it pops the arguments,
    /// hands the host safe Rust values, and pushes what comes back. That is
    /// what lets the whole VM subtree stay free of FFI and keep compiling for
    /// `wasm32-unknown-unknown` while still being one half of a hybrid program.
    ///
    /// Arguments are *borrowed* across the call — a string is handed over as a
    /// `&str` into this heap, never a copy — and the result is *owned*, because
    /// handing a value out is a move. The VM keeps ownership of every argument
    /// and drops them here, exactly as a callee frame's locals are dropped.
    fn call_native(&mut self, module: &Module, id: u32) -> Result<(), VmError> {
        let proto = module
            .functions
            .get(id as usize)
            .ok_or(VmError::UnknownFunction(id))?;
        let count = proto.param_count as usize;
        let first = self
            .stack
            .len()
            .checked_sub(count)
            .ok_or(VmError::StackUnderflow)?;
        let arguments = &self.stack[first..];

        // Borrowing the heap for the args while the host is borrowed mutably is
        // fine: they are disjoint fields of this struct.
        let mut lowered = Vec::with_capacity(count);
        for value in arguments {
            lowered.push(match *value {
                Value::Int(value) => NativeArg::Int(value),
                Value::Float(value) => NativeArg::Float(value),
                Value::Bool(value) => NativeArg::Bool(value),
                Value::Str(id) => NativeArg::Str(self.heap.get(id)),
                Value::Void => NativeArg::Void,
                // The hybrid ABI has no layout for a struct or an array yet, so
                // there is no honest way to hand one across. The split is
                // checked when the program is built, so this is the runtime
                // restating a rule rather than the first place it is enforced.
                Value::Struct(_) => return Err(VmError::StructAtSeam { function: id }),
                Value::Array(_) => return Err(VmError::ArrayAtSeam { function: id }),
                Value::Enum(_) => return Err(VmError::EnumAtSeam { function: id }),
            });
        }
        let returned = self
            .host
            .call_native(id, &lowered)
            .map_err(VmError::NativeCall);

        // The arguments were this frame's to own, whatever the call did.
        for value in self.stack.split_off(first) {
            self.heap.drop_value(value);
        }

        let result = self
            .heap
            .absorb(returned?)
            .ok_or(VmError::HandleAtSeam { function: id })?;
        self.stack.push(result);
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
                let old = std::mem::replace(&mut frame.locals[slot as usize], value);
                self.heap.drop_value(old);
            }
            Instruction::Pop => {
                let value = self.pop()?;
                self.heap.drop_value(value);
            }
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
                let Value::Struct(id) = base else {
                    self.heap.drop_value(base);
                    return Err(VmError::NotAStruct);
                };
                let field = self
                    .heap
                    .field(id, index)
                    .ok_or(VmError::NoSuchField { index })?;
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
                let len = self.heap.array_len(id).ok_or(VmError::NotAnArray)?;
                let count = i64::try_from(len).map_err(|_| VmError::ArrayTooLong)?;
                self.heap.drop_value(base);
                self.stack.push(Value::Int(count));
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
                let tag = self.heap.enum_tag(id).ok_or(VmError::NotAnEnum)?;
                self.heap.drop_value(base);
                self.stack.push(Value::Int(i64::from(tag)));
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
                let payload = self
                    .heap
                    .enum_payload(id)
                    .ok_or(VmError::MissingEnumPayload)?;
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

    fn pop_int(&mut self) -> Result<i64, VmError> {
        match self.pop()? {
            Value::Int(value) => Ok(value),
            _ => Err(VmError::TypeMismatch { expected: "Int" }),
        }
    }

    fn pop_float(&mut self) -> Result<f64, VmError> {
        match self.pop()? {
            Value::Float(value) => Ok(value),
            _ => Err(VmError::TypeMismatch { expected: "Float" }),
        }
    }

    fn pop_bool(&mut self) -> Result<bool, VmError> {
        match self.pop()? {
            Value::Bool(value) => Ok(value),
            _ => Err(VmError::TypeMismatch { expected: "Bool" }),
        }
    }

    fn pop_str(&mut self) -> Result<crate::value::StrId, VmError> {
        match self.pop()? {
            Value::Str(id) => Ok(id),
            _ => Err(VmError::TypeMismatch { expected: "String" }),
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
    })
}
