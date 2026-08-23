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
    /// Adds two integers without entering the general binary-op matcher.
    ///
    /// This is kept as a tiny always-inline helper because integer increments
    /// are the hottest arithmetic shape in generated loops.
    #[inline(always)]
    pub(super) fn add_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Int(lhs.wrapping_add(rhs)));
        Ok(())
    }

    /// Subtracts two integers without entering the general binary-op matcher.
    #[inline(always)]
    pub(super) fn sub_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Int(lhs.wrapping_sub(rhs)));
        Ok(())
    }

    /// Multiplies two integers without entering the general binary-op matcher.
    #[inline(always)]
    pub(super) fn mul_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Int(lhs.wrapping_mul(rhs)));
        Ok(())
    }

    /// Compares two integers without entering the general binary-op matcher.
    #[inline(always)]
    pub(super) fn lt_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs < rhs));
        Ok(())
    }

    /// Compares two integers for equality without entering the general
    /// binary-op matcher.
    #[inline(always)]
    pub(super) fn eq_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs == rhs));
        Ok(())
    }

    /// Compares two integers for inequality without entering the general
    /// binary-op matcher.
    #[inline(always)]
    pub(super) fn ne_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs != rhs));
        Ok(())
    }

    /// Performs a less-than-or-equal integer comparison without entering the
    /// general binary-op matcher.
    #[inline(always)]
    pub(super) fn le_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs <= rhs));
        Ok(())
    }

    /// Performs a greater-than integer comparison without entering the general
    /// binary-op matcher.
    #[inline(always)]
    pub(super) fn gt_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs > rhs));
        Ok(())
    }

    /// Performs a greater-than-or-equal integer comparison without entering
    /// the general binary-op matcher.
    #[inline(always)]
    pub(super) fn ge_int(&mut self) -> Result<(), VmError> {
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        self.stack.push(Value::Bool(lhs >= rhs));
        Ok(())
    }

    pub(super) fn binary(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        use Instruction as I;
        match instruction {
            I::AddInt | I::SubInt | I::MulInt | I::DivInt | I::RemInt | I::DivUInt | I::RemUInt => {
                self.int_arith(instruction)
            }
            I::AddFloat | I::SubFloat | I::MulFloat | I::DivFloat | I::RemFloat => {
                self.float_arith(instruction)
            }
            I::ConcatStr => self.concat(),
            I::EqInt | I::NeInt | I::LtInt | I::LeInt | I::GtInt | I::GeInt => {
                self.int_compare(instruction)
            }
            I::LtUInt | I::LeUInt | I::GtUInt | I::GeUInt => self.uint_compare(instruction),
            I::EqFloat | I::NeFloat | I::LtFloat | I::LeFloat | I::GtFloat | I::GeFloat => {
                self.float_compare(instruction)
            }
            I::EqBool | I::NeBool => self.bool_compare(instruction),
            I::EqStr | I::NeStr => self.str_compare(instruction),
            I::EqAny | I::NeAny => self.any_compare(instruction),
            I::BitAnd | I::BitOr | I::BitXor | I::Shl | I::ShrInt | I::ShrUInt => {
                self.bitwise(instruction)
            }
            _ => Err(VmError::BadDispatch),
        }
    }

    /// The bitwise operators and shifts, on the raw 64-bit pattern.
    ///
    /// Two rules the other backends mirror exactly. The shift amount is taken
    /// **modulo 64** rather than trapping or saturating, so `x << 64` is `x`;
    /// Rust's `wrapping_shl`/`wrapping_shr` mask the same way wasm's shifts do,
    /// while LLVM's `shl` would be poison, so the native backend masks
    /// explicitly to land here. And `>>` is the one shift with two forms:
    /// `ShrInt` propagates the sign bit, `ShrUInt` fills with zeros, which is
    /// why the compiler picks between them from the *left* operand's spelling
    /// rather than the VM inspecting anything.
    fn bitwise(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        let value = match instruction {
            I::BitAnd => lhs & rhs,
            I::BitOr => lhs | rhs,
            I::BitXor => lhs ^ rhs,
            I::Shl => lhs.wrapping_shl(rhs as u32),
            I::ShrInt => lhs.wrapping_shr(rhs as u32),
            I::ShrUInt => ((lhs as u64).wrapping_shr(rhs as u32)) as i64,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Int(value));
        Ok(())
    }

    fn int_arith(&mut self, instruction: &Instruction) -> Result<(), VmError> {
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
            // The `U8`..`U64` spellings: reinterpret the same 64 bits as
            // unsigned, divide, reinterpret back. No `wrapping_` needed —
            // unsigned division has no overflowing pair, which is why the
            // signed `MIN / -1` special case has no twin here.
            I::DivUInt => {
                if rhs == 0 {
                    return Err(VmError::DivideByZero);
                }
                ((lhs as u64) / (rhs as u64)) as i64
            }
            I::RemUInt => {
                if rhs == 0 {
                    return Err(VmError::DivideByZero);
                }
                ((lhs as u64) % (rhs as u64)) as i64
            }
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Int(value));
        Ok(())
    }

    fn float_arith(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_float()?;
        let lhs = self.pop_float()?;
        let value = match instruction {
            I::AddFloat => lhs + rhs,
            I::SubFloat => lhs - rhs,
            I::MulFloat => lhs * rhs,
            I::DivFloat => lhs / rhs,
            // Rust's `%` on `f64` is the truncated remainder `fmod` computes,
            // which is what LLVM's `frem` lowers to on the other engine.
            I::RemFloat => lhs % rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Float(value));
        Ok(())
    }

    fn concat(&mut self) -> Result<(), VmError> {
        let (lhs, rhs) = self.pop_two_str()?;
        let mut joined = String::with_capacity(self.heap.get(lhs).len() + self.heap.get(rhs).len());
        joined.push_str(self.heap.get(lhs));
        joined.push_str(self.heap.get(rhs));
        self.heap.free(lhs);
        self.heap.free(rhs);
        let id = self.heap.alloc(joined);
        self.stack.push(Value::Str(id));
        Ok(())
    }

    fn int_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
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

    /// Ordering for the `U8`..`U64` spellings.
    ///
    /// Equality has no unsigned twin and needs none: `==` on two 64-bit
    /// patterns is the same question under either signedness, so `EqInt`
    /// serves both and only the four orderings appear here.
    fn uint_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_int()? as u64;
        let lhs = self.pop_int()? as u64;
        let value = match instruction {
            I::LtUInt => lhs < rhs,
            I::LeUInt => lhs <= rhs,
            I::GtUInt => lhs > rhs,
            I::GeUInt => lhs >= rhs,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
        Ok(())
    }

    fn float_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
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

    fn bool_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
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

    /// Structural equality of two erased values.
    ///
    /// Erasure is the identity here — the VM's `Value` already carries its own
    /// tag, so an `Any` operand is just a value — which is why this pops two
    /// values of no particular kind rather than a pair of one kind. Both are
    /// dropped afterwards, as every comparison drops what it consumed; the
    /// comparison itself borrows the heap and takes nothing from it, so the
    /// drops are the only ownership this arm has to get right.
    fn any_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        let operands = self.pop_operands(2)?;
        let rhs = operands[0];
        let lhs = operands[1];
        let equal = self.heap.values_equal(lhs, rhs);
        for operand in operands {
            self.heap.drop_value(operand);
        }
        let value = match instruction {
            Instruction::EqAny => equal,
            Instruction::NeAny => !equal,
            _ => return Err(VmError::BadDispatch),
        };
        self.stack.push(Value::Bool(value));
        Ok(())
    }

    fn str_compare(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        let (lhs, rhs) = self.pop_two_str()?;
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
