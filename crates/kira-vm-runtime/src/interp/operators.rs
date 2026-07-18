//! The typed arithmetic, concatenation, and comparison operators.
//!
//! Split out of the dispatch loop, which stays one match-in-loop for speed.
//! These are the operators whose *typed* spelling is the contract: the compiler
//! picks `AddInt` or `AddFloat` at compile time, so nothing here inspects a
//! value's type to decide what an operator means — a mismatch is a compiler
//! bug, reported as one, never coerced.
//!
//! Wrapping is deliberate on every integer operation, and it is what the LLVM
//! and wasm backends mirror: `Int` arithmetic wraps rather than trapping, and
//! only division by zero traps.

use kira_bytecode::op::Instruction;

use crate::error::VmError;
use crate::interp::Vm;
use crate::value::Value;

impl Vm<'_> {
    pub(super) fn binary(&mut self, instruction: Instruction) -> Result<(), VmError> {
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
}
