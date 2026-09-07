//! Integer width and conversion semantics.
//!
//! An integer is one 64-bit word on the stack whatever its spelling, so the
//! width rules are instructions of their own: checked arithmetic that traps
//! where the wrapping opcodes wrap, a range check after arithmetic at a
//! narrower width, a shift-count check, and a checked conversion between
//! spellings. Each failure is a [`VmError`] the native engine raises the same
//! way, so a program traps identically on both.

use kira_bytecode::op::Instruction;
use kira_runtime_abi::IntWidth;

use super::Vm;
use crate::error::VmError;
use crate::value::Value;

/// The width a one-byte code names; a bad code is a malformed module.
fn spelling(code: u8) -> Result<IntWidth, VmError> {
    IntWidth::from_code(code).ok_or(VmError::BadDispatch)
}

impl Vm<'_> {
    /// Signed or unsigned 64-bit arithmetic that traps on overflow.
    pub(super) fn checked_int_arith(&mut self, instruction: &Instruction) -> Result<(), VmError> {
        use Instruction as I;
        let rhs = self.pop_int()?;
        let lhs = self.pop_int()?;
        let (value, spelling) = match instruction {
            I::AddIntChecked => (lhs.checked_add(rhs), "Int"),
            I::SubIntChecked => (lhs.checked_sub(rhs), "Int"),
            I::MulIntChecked => (lhs.checked_mul(rhs), "Int"),
            I::DivIntChecked => {
                if rhs == 0 {
                    return Err(VmError::DivideByZero);
                }
                (lhs.checked_div(rhs), "Int")
            }
            I::AddUIntChecked => (
                (lhs as u64).checked_add(rhs as u64).map(|v| v as i64),
                "U64",
            ),
            I::SubUIntChecked => (
                (lhs as u64).checked_sub(rhs as u64).map(|v| v as i64),
                "U64",
            ),
            I::MulUIntChecked => (
                (lhs as u64).checked_mul(rhs as u64).map(|v| v as i64),
                "U64",
            ),
            _ => return Err(VmError::BadDispatch),
        };
        let value = value.ok_or(VmError::IntegerOverflow { spelling })?;
        self.stack.push(Value::Int(value));
        Ok(())
    }

    /// Negation that traps on `i64::MIN`.
    pub(super) fn neg_int_checked(&mut self) -> Result<(), VmError> {
        let value = self.pop_int()?;
        let value = value
            .checked_neg()
            .ok_or(VmError::IntegerOverflow { spelling: "Int" })?;
        self.stack.push(Value::Int(value));
        Ok(())
    }

    /// Traps unless the word on top, read as a signed 64-bit result, lies in
    /// the range of the spelling `code` names.
    pub(super) fn check_int(&mut self, code: u8) -> Result<(), VmError> {
        let spelling = spelling(code)?;
        let value = self.pop_int()?;
        if !spelling.holds(i128::from(value)) {
            return Err(VmError::IntegerOverflow {
                spelling: spelling.name(),
            });
        }
        self.stack.push(Value::Int(value));
        Ok(())
    }

    /// Reduces the word on top to the width of the spelling `code` names.
    pub(super) fn wrap_int(&mut self, code: u8) -> Result<(), VmError> {
        let spelling = spelling(code)?;
        let value = self.pop_int()?;
        self.stack.push(Value::Int(spelling.wrap(value)));
        Ok(())
    }

    /// Traps unless the shift count on top lies in `0..bits`; the count stays
    /// on the stack for the shift that follows.
    pub(super) fn check_shift(&mut self, bits: u8) -> Result<(), VmError> {
        let bits = u32::from(bits);
        let count = self.pop_int()?;
        if count < 0 || count >= i64::from(bits) {
            return Err(VmError::ShiftOutOfRange { count, bits });
        }
        self.stack.push(Value::Int(count));
        Ok(())
    }

    /// Converts the word on top from spelling `from` to spelling `to`,
    /// trapping when the value is not representable at the destination.
    pub(super) fn convert_int(&mut self, from: u8, to: u8) -> Result<(), VmError> {
        let (from, to) = (spelling(from)?, spelling(to)?);
        let word = self.pop_int()?;
        let value = from.value_of(word);
        if !to.holds(value) {
            return Err(VmError::NarrowingOutOfRange {
                value,
                spelling: to.name(),
            });
        }
        self.stack.push(Value::Int(to.word_of(value)));
        Ok(())
    }

    /// Truncates the float on top toward zero, trapping on NaN, an infinity,
    /// or a value outside the 64-bit signed range.
    pub(super) fn convert_float_to_int(&mut self) -> Result<(), VmError> {
        let value = self.pop_float()?;
        // The truncated value must lie in `[i64::MIN, i64::MAX]`; `2^63` as a
        // float is exactly the first value past the top, so it is excluded by
        // a strict compare, while `-2^63` is exactly `i64::MIN` and included.
        let in_range = value.is_finite()
            && (-9_223_372_036_854_775_808.0..9_223_372_036_854_775_808.0).contains(&value);
        if !in_range {
            return Err(VmError::FloatToIntOutOfRange { value });
        }
        self.stack.push(Value::Int(value as i64));
        Ok(())
    }

    /// Truncates the float on top toward zero as a `U64`, trapping on NaN, an
    /// infinity, or a value outside `0..2^64`.
    ///
    /// `U64` is the one integer destination whose range is not a subrange of
    /// `Int`'s, so it has its own conversion: reading the float as signed first
    /// would refuse every value above `2^63`, which is half of what a `U64`
    /// holds. The word pushed is the unsigned value's bit pattern, which is how
    /// a `U64` is carried everywhere else.
    pub(super) fn convert_float_to_uint(&mut self) -> Result<(), VmError> {
        let value = self.pop_float()?;
        // `2^64` as a float is exactly the first value past the top, so the
        // strict upper compare excludes it. The lower bound is not strict, so
        // `-0.0` is accepted and converts to zero, while every negative
        // magnitude — `-0.5` included, which would otherwise truncate to zero
        // and hide a sign error — is refused before the truncation happens.
        let in_range = value.is_finite() && (0.0..18_446_744_073_709_551_616.0).contains(&value);
        if !in_range {
            return Err(VmError::FloatToIntOutOfRange { value });
        }
        self.stack.push(Value::Int(value as u64 as i64));
        Ok(())
    }
}
