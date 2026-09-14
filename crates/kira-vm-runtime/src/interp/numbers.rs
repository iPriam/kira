//! The interpreter's `Number` operations.
//!
//! Every operation delegates to [`kira_runtime_abi::Decimal`], the one exact
//! decimal both this VM and the native runtime compute with, so the two cannot
//! disagree. This file only marshals operands out of `Value`s, turns a
//! [`DecimalError`] into a trap, and reclaims the operands it was handed.

use kira_runtime_abi::{Decimal, DecimalError, NumberOp};

use crate::error::VmError;
use crate::interp::Vm;
use crate::value::Value;

/// The decimal a `Value` holds, or a type-mismatch trap.
fn as_number(value: Value) -> Result<Decimal, VmError> {
    match value {
        Value::Number(decimal) => Ok(decimal),
        _ => Err(VmError::TypeMismatch {
            expected: "Number",
        }),
    }
}

impl Vm<'_> {
    /// One `Number` operation, dropping every operand on every path out.
    ///
    /// `operands` are in source order; this owns them and reclaims each before
    /// it returns, failing paths included, exactly as `perform_string_op` does.
    pub(super) fn perform_number_op(
        &mut self,
        op: NumberOp,
        operands: &[Value],
    ) -> Result<Value, VmError> {
        let performed = self.number_op_result(op, operands);
        for &operand in operands {
            self.heap.drop_value(operand);
        }
        performed
    }

    /// The operation itself, leaving the operands for the caller to reclaim.
    fn number_op_result(&mut self, op: NumberOp, operands: &[Value]) -> Result<Value, VmError> {
        let trap = |error: DecimalError| VmError::NumberTrap(error.message());
        match op {
            NumberOp::FromInt => match operands {
                [Value::Int(value)] => Ok(Value::Number(Decimal::from_i64(*value))),
                _ => Err(VmError::TypeMismatch { expected: "Int" }),
            },
            NumberOp::FromString => match operands {
                [Value::Str(id)] => {
                    Decimal::parse(self.heap.get(*id)).map(Value::Number).map_err(trap)
                }
                _ => Err(VmError::NotAString),
            },
            NumberOp::FromFloat => match operands {
                [Value::Float(value)] => {
                    Decimal::from_f64(*value).map(Value::Number).map_err(trap)
                }
                _ => Err(VmError::TypeMismatch { expected: "Float" }),
            },
            NumberOp::ToString => {
                let decimal = as_number(operands[0])?;
                Ok(Value::Str(self.heap.alloc(decimal.to_decimal_string())))
            }
            NumberOp::ToInt => as_number(operands[0])?.to_i64().map(Value::Int).map_err(trap),
            NumberOp::ToFloat => Ok(Value::Float(as_number(operands[0])?.to_f64())),
            NumberOp::Negate => Ok(Value::Number(as_number(operands[0])?.negate())),
            NumberOp::Add => self.number_binary(operands, Decimal::add),
            NumberOp::Subtract => self.number_binary(operands, Decimal::subtract),
            NumberOp::Multiply => self.number_binary(operands, Decimal::multiply),
            NumberOp::Divide => self.number_binary(operands, Decimal::divide),
            NumberOp::Equal => self.number_predicate(operands, |a, b| a.equals(b)),
            NumberOp::Less => self.number_predicate(operands, |a, b| Ok(a.compare(b)?.is_lt())),
            NumberOp::LessOrEqual => self.number_predicate(operands, |a, b| Ok(a.compare(b)?.is_le())),
            NumberOp::Greater => self.number_predicate(operands, |a, b| Ok(a.compare(b)?.is_gt())),
            NumberOp::GreaterOrEqual => {
                self.number_predicate(operands, |a, b| Ok(a.compare(b)?.is_ge()))
            }
        }
    }

    /// A binary arithmetic operation, answering a `Number`.
    fn number_binary(
        &mut self,
        operands: &[Value],
        combine: impl Fn(Decimal, Decimal) -> Result<Decimal, DecimalError>,
    ) -> Result<Value, VmError> {
        let left = as_number(operands[0])?;
        let right = as_number(operands[1])?;
        combine(left, right)
            .map(Value::Number)
            .map_err(|error| VmError::NumberTrap(error.message()))
    }

    /// A binary comparison, answering a `Bool`.
    fn number_predicate(
        &mut self,
        operands: &[Value],
        compare: impl Fn(Decimal, Decimal) -> Result<bool, DecimalError>,
    ) -> Result<Value, VmError> {
        let left = as_number(operands[0])?;
        let right = as_number(operands[1])?;
        compare(left, right)
            .map(Value::Bool)
            .map_err(|error| VmError::NumberTrap(error.message()))
    }
}
