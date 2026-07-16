//! The bytecode interpreter: a match-in-loop stack machine.
//!
//! The interpreter keeps call frames on a heap-allocated stack (so Kira
//! recursion never consumes the host's native stack) and a single shared
//! operand stack. It touches the outside world only through the
//! [`HostCapabilities`] trait, so the whole crate stays portable to
//! `wasm32-unknown-unknown`.

use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction;
use kira_runtime_abi::HostCapabilities;

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
    // A Module is a public, deserializable artifact: prove every index and
    // operand in range before trusting it, so interpretation cannot panic.
    module.validate()?;
    let mut vm = Vm {
        host,
        heap: Heap::new(),
        stack: Vec::new(),
    };
    let result = vm.run(module)?;
    // The program's result is no longer referenced by anything; drop it so
    // heap accounting reflects a fully reclaimed program.
    vm.heap.drop_value(result);
    Ok(RunOutcome {
        result,
        heap: vm.heap.stats(),
    })
}

struct Vm<'h> {
    host: &'h mut dyn HostCapabilities,
    heap: Heap,
    stack: Vec<Value>,
}

impl Vm<'_> {
    fn run(&mut self, module: &Module) -> Result<Value, VmError> {
        let main = module.main;
        let mut frames = vec![new_frame(module, main)?];

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
                let line = self.heap.format_and_consume(value);
                self.host.write_line(&line);
                self.stack.push(Value::Void);
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
