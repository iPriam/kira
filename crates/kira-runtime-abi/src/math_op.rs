//! The floating-point operations that travel as one opcode with an operand byte.
//!
//! These are the functions a program cannot write for itself. A square root or
//! a sine derived in Kira is a Taylor series or a Newton iteration — the
//! foundation shipped `sqrtApprox`, `sinApprox` and `cosApprox`, and the names
//! said what they were. Every target already has these: an x86 `sqrtsd`, an
//! LLVM intrinsic, a libm call. Spending a series expansion to reach an answer
//! the hardware has is the kind of thing a language does once and regrets.
//!
//! They share a single `MathOp` instruction and are told apart by the byte that
//! follows it, exactly as [`StringOp`](crate::StringOp) and
//! [`FileSystemOp`](crate::FileSystemOp) already are, so a new operation costs a
//! number in this enum and nothing in the one-byte opcode table.

/// Which floating-point operation one `MathOp` instruction performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `MathOp` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MathOp {
    /// The non-negative square root.
    Sqrt = 0,
    /// The sine of an angle in radians.
    Sin = 1,
    /// The cosine of an angle in radians.
    Cos = 2,
    /// The tangent of an angle in radians.
    Tan = 3,
    /// The largest integer not greater than the value.
    Floor = 4,
    /// The smallest integer not less than the value.
    Ceil = 5,
    /// The magnitude, without its sign.
    Abs = 6,
}

impl MathOp {
    /// The name a Kira program calls this operation by.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Sqrt => "sqrt",
            Self::Sin => "sin",
            Self::Cos => "cos",
            Self::Tan => "tan",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
            Self::Abs => "abs",
        }
    }

    /// The operation a Kira program spells `name`, if it spells one.
    pub fn from_name(name: &str) -> Option<Self> {
        ALL.iter().copied().find(|op| op.name() == name)
    }

    /// This operation's operand byte.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// The operation an operand byte names.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Sqrt),
            1 => Some(Self::Sin),
            2 => Some(Self::Cos),
            3 => Some(Self::Tan),
            4 => Some(Self::Floor),
            5 => Some(Self::Ceil),
            6 => Some(Self::Abs),
            _ => None,
        }
    }

    /// Applies the operation, for an engine evaluating one directly.
    ///
    /// Every engine routes through this, so the VM and a constant folded at
    /// compile time cannot disagree about what `sqrt` means.
    pub fn apply(self, value: f64) -> f64 {
        match self {
            Self::Sqrt => value.sqrt(),
            Self::Sin => value.sin(),
            Self::Cos => value.cos(),
            Self::Tan => value.tan(),
            Self::Floor => value.floor(),
            Self::Ceil => value.ceil(),
            Self::Abs => value.abs(),
        }
    }
}

/// Every operation, for name lookup and for a test that covers them all.
pub const ALL: [MathOp; 7] = [
    MathOp::Sqrt,
    MathOp::Sin,
    MathOp::Cos,
    MathOp::Tan,
    MathOp::Floor,
    MathOp::Ceil,
    MathOp::Abs,
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_round_trips_through_its_tag_and_its_name() {
        for op in ALL {
            assert_eq!(MathOp::from_tag(op.tag()), Some(op));
            assert_eq!(MathOp::from_name(op.name()), Some(op));
        }
        assert_eq!(MathOp::from_tag(ALL.len() as u8), None);
        assert_eq!(MathOp::from_name("nope"), None);
    }

    /// A square root a program cannot get wrong is the point of having one.
    ///
    /// The foundation's eight-iteration Newton `sqrtApprox` was accurate to
    /// about six digits; this is exact to the last bit.
    #[test]
    fn a_square_root_is_exact_rather_than_approximated() {
        assert!((MathOp::Sqrt.apply(2.0) - std::f64::consts::SQRT_2).abs() < f64::EPSILON);
        assert_eq!(MathOp::Sqrt.apply(144.0), 12.0);
    }

    #[test]
    fn the_trigonometric_operations_agree_with_the_unit_circle() {
        let pi = std::f64::consts::PI;
        assert!(MathOp::Sin.apply(0.0).abs() < 1e-12);
        assert!((MathOp::Cos.apply(0.0) - 1.0).abs() < 1e-12);
        assert!((MathOp::Sin.apply(pi / 2.0) - 1.0).abs() < 1e-12);
        assert!(MathOp::Tan.apply(0.0).abs() < 1e-12);
    }

    #[test]
    fn rounding_and_magnitude_answer_for_negatives_too() {
        assert_eq!(MathOp::Floor.apply(-1.5), -2.0);
        assert_eq!(MathOp::Ceil.apply(-1.5), -1.0);
        assert_eq!(MathOp::Abs.apply(-1.5), 1.5);
    }
}
