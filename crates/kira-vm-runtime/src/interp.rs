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
    };
    let result = vm.enter(module, module.main, &[])?;
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
            frame.locals[slot] = self.heap.lower(*argument);
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
                // The hybrid ABI has no layout for a struct yet, so there is no
                // honest way to hand one across. The split is checked when the
                // program is built, so this is the runtime restating a rule
                // rather than the first place it is enforced.
                Value::Struct(_) => return Err(VmError::StructAtSeam { function: id }),
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

        let result = self.heap.absorb(returned?);
        self.stack.push(result);
        Ok(())
    }

    /// Writes `value` into local `slot`, walking `path` field by field.
    ///
    /// Walking moves *handles*, never objects: each step reads the nested
    /// struct's handle out of its parent, so the write lands in the same object
    /// the local holds. Nothing is copied and nothing is rebuilt, which is what
    /// makes `b.size.x = 1` a write rather than a reconstruction of `b`. That
    /// is only sound because a struct's fields are exclusively owned — the deep
    /// copy on every read is what guarantees no other value shares them.
    fn store_field(
        &mut self,
        frame: &mut Frame,
        slot: u16,
        path: &[u16],
        value: Value,
    ) -> Result<(), VmError> {
        let Some((&last, walk)) = path.split_last() else {
            return Err(VmError::EmptyFieldPath);
        };
        let mut current = frame.locals[slot as usize];
        for &step in walk {
            let Value::Struct(id) = current else {
                return Err(VmError::NotAStruct);
            };
            current = self
                .heap
                .field(id, step)
                .ok_or(VmError::NoSuchField { index: step })?;
        }
        let Value::Struct(id) = current else {
            return Err(VmError::NotAStruct);
        };
        if !self.heap.set_field(id, last, value) {
            return Err(VmError::NoSuchField { index: last });
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
                if let Err(error) = self.store_field(frame, slot, path.steps(), value) {
                    // The value was ours the moment it left the stack, so a
                    // failed write frees it rather than leaking it.
                    self.heap.drop_value(value);
                    return Err(error);
                }
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

    fn binary(&mut self, instruction: Instruction) -> Result<(), VmError> {
        use Instruction as I;
        match instruction {
            I::AddInt | I::SubInt | I::MulInt | I::DivInt | I::RemInt => {
                self.int_arith(instruction)
            }
            I::AddFloat | I::SubFloat | I::MulFloat | I::DivFloat => self.float_arith(instruction),
            I::ConcatStr => self.concat(),
            I::EqInt | I::NeInt | I::LtInt | I::LeInt | I::GtInt | I::GeInt => {
                self.int_compare(instruction)
            }
            I::EqFloat | I::NeFloat | I::LtFloat | I::LeFloat | I::GtFloat | I::GeFloat => {
                self.float_compare(instruction)
            }
            I::EqBool | I::NeBool => self.bool_compare(instruction),
            I::EqStr | I::NeStr => self.str_compare(instruction),
            _ => Err(VmError::BadDispatch),
        }
    }

    fn int_arith(&mut self, instruction: Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        let value = match instruction {
            I::AddInt => lhs.wrapping_add(rhs),
            I::SubInt => lhs.wrapping_sub(rhs),
            I::MulInt => lhs.wrapping_mul(rhs),
            I::DivInt => {
                if rhs == 0 {
                    return Err(VmError::DivideByZero);
                }
                lhs.wrapping_div(rhs)
            }
            I::RemInt => {
                if rhs == 0 {
                    return Err(VmError::DivideByZero);
                }
                lhs.wrapping_rem(rhs)
            }
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Int(value));
        Ok(())
    }

    fn float_arith(&mut self, instruction: Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_float()?;
        let lhs = self.pop_float()?;
        let value = match instruction {
            I::AddFloat => lhs + rhs,
            I::SubFloat => lhs - rhs,
            I::MulFloat => lhs * rhs,
            I::DivFloat => lhs / rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Float(value));
        Ok(())
    }

    fn concat(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_str()?;
        let lhs = self.pop_str()?;
        let mut joined = String::with_capacity(self.heap.get(lhs).len() + self.heap.get(rhs).len());
        joined.push_str(self.heap.get(lhs));
        joined.push_str(self.heap.get(rhs));
        self.heap.free(lhs);
        self.heap.free(rhs);
        let id = self.heap.alloc(joined);
        self.stack.push(Value::Str(id));
        Ok(())
    }

    fn int_compare(&mut self, instruction: Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        let value = match instruction {
            I::EqInt => lhs == rhs,
            I::NeInt => lhs != rhs,
            I::LtInt => lhs < rhs,
            I::LeInt => lhs <= rhs,
            I::GtInt => lhs > rhs,
            I::GeInt => lhs >= rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
        Ok(())
    }

    fn float_compare(&mut self, instruction: Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_float()?;
        let lhs = self.pop_float()?;
        let value = match instruction {
            I::EqFloat => lhs == rhs,
            I::NeFloat => lhs != rhs,
            I::LtFloat => lhs < rhs,
            I::LeFloat => lhs <= rhs,
            I::GtFloat => lhs > rhs,
            I::GeFloat => lhs >= rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
        Ok(())
    }

    fn bool_compare(&mut self, instruction: Instruction) -> Result<(), VmError> {
        let rhs = self.pop_bool()?;
        let lhs = self.pop_bool()?;
        let value = match instruction {
            Instruction::EqBool => lhs == rhs,
            Instruction::NeBool => lhs != rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
        Ok(())
    }

    fn str_compare(&mut self, instruction: Instruction) -> Result<(), VmError> {
        let rhs = self.pop_str()?;
        let lhs = self.pop_str()?;
        let equal = self.heap.get(lhs) == self.heap.get(rhs);
        self.heap.free(lhs);
        self.heap.free(rhs);
        let value = match instruction {
            Instruction::EqStr => equal,
            Instruction::NeStr => !equal,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
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
