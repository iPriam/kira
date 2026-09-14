//! The `Number` operations that travel as one opcode with an operand byte.
//!
//! `Number` is Kira's exact base-10 decimal: an `i128` mantissa and a decimal
//! scale, so `0.1 + 0.2` is `0.3` rather than the nearest binary float. Like the
//! string surface ([`StringOp`](crate::StringOp)) every operation on it shares a
//! single bytecode instruction and is told apart by the byte that follows, so a
//! new one costs a number here and nothing in the opcode table.
//!
//! The discriminants are a wire contract: they travel in the operand byte, so
//! they are **append-only** — a new operation takes the next free number and no
//! existing one moves.

/// Which `Number` operation one instruction performs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum NumberOp {
    /// An `Int` as an exact `Number` (`Number(i)`).
    FromInt = 0,
    /// A decimal string parsed to an exact `Number` (`Number(s)`), trapping when
    /// the text does not read as a decimal.
    FromString = 1,
    /// A `Float` as the `Number` nearest its value (`Number(f)`). Lossy by
    /// nature — the float was already rounded — and named a conversion for it.
    FromFloat = 2,
    /// The `Number` rendered as text (`String(n)`), shortest exact decimal.
    ToString = 3,
    /// The `Number` truncated toward zero to an `Int`, trapping on overflow.
    ToInt = 4,
    /// The `Number` as the nearest `Float`.
    ToFloat = 5,
    /// Sum of two `Number`s, exact.
    Add = 6,
    /// Difference of two `Number`s, exact.
    Subtract = 7,
    /// Product of two `Number`s, exact.
    Multiply = 8,
    /// Quotient of two `Number`s, rounded half-to-even at the working scale,
    /// trapping on division by zero.
    Divide = 9,
    /// A `Number` negated.
    Negate = 10,
    /// Whether two `Number`s are numerically equal (`1.0` equals `1.00`).
    Equal = 11,
    /// Whether the first `Number` is strictly less than the second.
    Less = 12,
    /// Whether the first is less than or equal to the second.
    LessOrEqual = 13,
    /// Whether the first is strictly greater than the second.
    Greater = 14,
    /// Whether the first is greater than or equal to the second.
    GreaterOrEqual = 15,
}

impl NumberOp {
    /// Every operation, in wire order. Decoding indexes this, so a new operation
    /// cannot be added to the enum and forgotten by the decoder.
    pub const ALL: [NumberOp; 16] = [
        NumberOp::FromInt,
        NumberOp::FromString,
        NumberOp::FromFloat,
        NumberOp::ToString,
        NumberOp::ToInt,
        NumberOp::ToFloat,
        NumberOp::Add,
        NumberOp::Subtract,
        NumberOp::Multiply,
        NumberOp::Divide,
        NumberOp::Negate,
        NumberOp::Equal,
        NumberOp::Less,
        NumberOp::LessOrEqual,
        NumberOp::Greater,
        NumberOp::GreaterOrEqual,
    ];

    /// The wire byte this operation travels as.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    #[must_use]
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    #[must_use]
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            NumberOp::FromInt => "kira_rt_number_from_int",
            NumberOp::FromString => "kira_rt_number_from_string",
            NumberOp::FromFloat => "kira_rt_number_from_float",
            NumberOp::ToString => "kira_rt_number_to_string",
            NumberOp::ToInt => "kira_rt_number_to_int",
            NumberOp::ToFloat => "kira_rt_number_to_float",
            NumberOp::Add => "kira_rt_number_add",
            NumberOp::Subtract => "kira_rt_number_subtract",
            NumberOp::Multiply => "kira_rt_number_multiply",
            NumberOp::Divide => "kira_rt_number_divide",
            NumberOp::Negate => "kira_rt_number_negate",
            NumberOp::Equal => "kira_rt_number_equal",
            NumberOp::Less => "kira_rt_number_less",
            NumberOp::LessOrEqual => "kira_rt_number_less_or_equal",
            NumberOp::Greater => "kira_rt_number_greater",
            NumberOp::GreaterOrEqual => "kira_rt_number_greater_or_equal",
        }
    }

    /// How many operands the operation takes off the stack.
    ///
    /// The conversions in and out and negation are unary; the arithmetic and the
    /// comparisons are binary.
    #[must_use]
    pub const fn operand_count(self) -> usize {
        match self {
            NumberOp::FromInt
            | NumberOp::FromString
            | NumberOp::FromFloat
            | NumberOp::ToString
            | NumberOp::ToInt
            | NumberOp::ToFloat
            | NumberOp::Negate => 1,
            NumberOp::Add
            | NumberOp::Subtract
            | NumberOp::Multiply
            | NumberOp::Divide
            | NumberOp::Equal
            | NumberOp::Less
            | NumberOp::LessOrEqual
            | NumberOp::Greater
            | NumberOp::GreaterOrEqual => 2,
        }
    }

    /// Whether the operation answers a `Bool` — the comparisons.
    #[must_use]
    pub const fn answers_bool(self) -> bool {
        matches!(
            self,
            NumberOp::Equal
                | NumberOp::Less
                | NumberOp::LessOrEqual
                | NumberOp::Greater
                | NumberOp::GreaterOrEqual
        )
    }

    /// Whether the operation answers a `Number`.
    #[must_use]
    pub const fn answers_number(self) -> bool {
        matches!(
            self,
            NumberOp::FromInt
                | NumberOp::FromString
                | NumberOp::FromFloat
                | NumberOp::Add
                | NumberOp::Subtract
                | NumberOp::Multiply
                | NumberOp::Divide
                | NumberOp::Negate
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_bytes_are_pinned() {
        assert_eq!(NumberOp::FromInt.as_byte(), 0);
        assert_eq!(NumberOp::ToString.as_byte(), 3);
        assert_eq!(NumberOp::Add.as_byte(), 6);
        assert_eq!(NumberOp::Divide.as_byte(), 9);
        assert_eq!(NumberOp::GreaterOrEqual.as_byte(), 15);
    }

    #[test]
    fn every_operation_round_trips_its_byte() {
        for op in NumberOp::ALL {
            assert_eq!(NumberOp::from_byte(op.as_byte()), Some(op));
        }
    }

    #[test]
    fn an_unknown_byte_names_no_operation() {
        assert_eq!(NumberOp::from_byte(NumberOp::ALL.len() as u8), None);
        assert_eq!(NumberOp::from_byte(u8::MAX), None);
    }

    #[test]
    fn each_operation_names_a_distinct_runtime_symbol() {
        let mut seen: Vec<&str> = NumberOp::ALL.iter().map(|op| op.runtime_symbol()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two operations share a symbol");
    }
}
