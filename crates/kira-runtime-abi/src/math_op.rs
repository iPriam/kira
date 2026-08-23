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
//!
//! An operation takes one operand or two, and which it takes follows from the
//! operation — [`MathOp::argument_count`], the same way `StringOp` answers for
//! its own methods. `pow(x, y)` and `min(a, b)` are as much the target's own
//! maths as `sqrt` is, and a surface that could only carry unary operations
//! would have sent them back to being written in Kira.
//!
//! Every operation here is `Float`-valued and `Float`-taking. That is what
//! makes them one instruction: `min` on two `Int`s is integer minimum, a
//! different operation on a different unit, and it does not belong in a table
//! whose whole contract is `double(double...)`.

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
    /// `e` raised to the value.
    Exp = 7,
    /// The natural logarithm, to base `e`.
    Log = 8,
    /// The logarithm to base 2.
    Log2 = 9,
    /// The logarithm to base 10.
    Log10 = 10,
    /// 2 raised to the value.
    Exp2 = 11,
    /// The nearest integer, halves rounded away from zero.
    Round = 12,
    /// The integer part, the fraction dropped toward zero.
    Trunc = 13,
    /// The arc sine, in radians.
    Asin = 14,
    /// The arc cosine, in radians.
    Acos = 15,
    /// The arc tangent, in radians.
    Atan = 16,
    /// The hyperbolic sine.
    Sinh = 17,
    /// The hyperbolic cosine.
    Cosh = 18,
    /// The hyperbolic tangent.
    Tanh = 19,
    /// The first operand raised to the second.
    Pow = 20,
    /// The angle of the point `(second, first)` from the positive x axis, in
    /// radians — `atan2(y, x)`, taking the quadrant from both signs.
    Atan2 = 21,
    /// The smaller of two values.
    Min = 22,
    /// The larger of two values.
    Max = 23,
    /// The length of the hypotenuse of a right triangle with these sides,
    /// computed without the overflow that squaring them would risk.
    Hypot = 24,
    /// The first operand's magnitude with the second operand's sign.
    CopySign = 25,
    /// The remainder of the first operand divided by the second, truncated
    /// toward zero — C's `fmod`, and what `%` means on floats.
    Fmod = 26,
}

impl MathOp {
    /// The most operands any operation takes.
    ///
    /// An engine sizes its operand buffer by this rather than by a number of
    /// its own, so adding a three-operand operation — a fused multiply-add — is
    /// a change here and nowhere else. The test below is what keeps it true.
    pub const MAX_ARGUMENTS: usize = 2;

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
            Self::Exp => "exp",
            Self::Log => "log",
            Self::Log2 => "log2",
            Self::Log10 => "log10",
            Self::Exp2 => "exp2",
            Self::Round => "round",
            Self::Trunc => "trunc",
            Self::Asin => "asin",
            Self::Acos => "acos",
            Self::Atan => "atan",
            Self::Sinh => "sinh",
            Self::Cosh => "cosh",
            Self::Tanh => "tanh",
            Self::Pow => "pow",
            Self::Atan2 => "atan2",
            Self::Min => "min",
            Self::Max => "max",
            Self::Hypot => "hypot",
            Self::CopySign => "copysign",
            Self::Fmod => "fmod",
        }
    }

    /// The operation a Kira program spells `name`, if it spells one.
    pub fn from_name(name: &str) -> Option<Self> {
        ALL.iter().copied().find(|op| op.name() == name)
    }

    /// How many operands the operation takes.
    ///
    /// The count is the operation's own, not a property of the instruction:
    /// one `MathOp` opcode carries every arity, and the operand byte is what
    /// says how many values were pushed before it.
    pub const fn argument_count(self) -> usize {
        match self {
            Self::Pow
            | Self::Atan2
            | Self::Min
            | Self::Max
            | Self::Hypot
            | Self::CopySign
            | Self::Fmod => 2,
            _ => 1,
        }
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
            7 => Some(Self::Exp),
            8 => Some(Self::Log),
            9 => Some(Self::Log2),
            10 => Some(Self::Log10),
            11 => Some(Self::Exp2),
            12 => Some(Self::Round),
            13 => Some(Self::Trunc),
            14 => Some(Self::Asin),
            15 => Some(Self::Acos),
            16 => Some(Self::Atan),
            17 => Some(Self::Sinh),
            18 => Some(Self::Cosh),
            19 => Some(Self::Tanh),
            20 => Some(Self::Pow),
            21 => Some(Self::Atan2),
            22 => Some(Self::Min),
            23 => Some(Self::Max),
            24 => Some(Self::Hypot),
            25 => Some(Self::CopySign),
            26 => Some(Self::Fmod),
            _ => None,
        }
    }

    /// Applies the operation to its operands, in source order.
    ///
    /// Every engine routes through this, so the VM and a constant folded at
    /// compile time cannot disagree about what `sqrt` means. Operands beyond
    /// the operation's own [`argument_count`](Self::argument_count) are not
    /// read, and a call with too few is a caller that did not typecheck.
    pub fn apply(self, operands: &[f64]) -> f64 {
        debug_assert!(
            operands.len() >= self.argument_count(),
            "`{}` takes {} operand(s) and was applied to {}",
            self.name(),
            self.argument_count(),
            operands.len()
        );
        let a = operands[0];
        match self {
            Self::Sqrt => a.sqrt(),
            Self::Sin => a.sin(),
            Self::Cos => a.cos(),
            Self::Tan => a.tan(),
            Self::Floor => a.floor(),
            Self::Ceil => a.ceil(),
            Self::Abs => a.abs(),
            Self::Exp => a.exp(),
            Self::Log => a.ln(),
            Self::Log2 => a.log2(),
            Self::Log10 => a.log10(),
            Self::Exp2 => a.exp2(),
            Self::Round => a.round(),
            Self::Trunc => a.trunc(),
            Self::Asin => a.asin(),
            Self::Acos => a.acos(),
            Self::Atan => a.atan(),
            Self::Sinh => a.sinh(),
            Self::Cosh => a.cosh(),
            Self::Tanh => a.tanh(),
            Self::Pow => a.powf(operands[1]),
            Self::Atan2 => a.atan2(operands[1]),
            Self::Min => a.min(operands[1]),
            Self::Max => a.max(operands[1]),
            Self::Hypot => a.hypot(operands[1]),
            Self::CopySign => a.copysign(operands[1]),
            Self::Fmod => a % operands[1],
        }
    }
}

/// Every operation, for name lookup and for a test that covers them all.
pub const ALL: [MathOp; 27] = [
    MathOp::Sqrt,
    MathOp::Sin,
    MathOp::Cos,
    MathOp::Tan,
    MathOp::Floor,
    MathOp::Ceil,
    MathOp::Abs,
    MathOp::Exp,
    MathOp::Log,
    MathOp::Log2,
    MathOp::Log10,
    MathOp::Exp2,
    MathOp::Round,
    MathOp::Trunc,
    MathOp::Asin,
    MathOp::Acos,
    MathOp::Atan,
    MathOp::Sinh,
    MathOp::Cosh,
    MathOp::Tanh,
    MathOp::Pow,
    MathOp::Atan2,
    MathOp::Min,
    MathOp::Max,
    MathOp::Hypot,
    MathOp::CopySign,
    MathOp::Fmod,
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

    /// The tags are a wire contract, so the test states the numbers rather than
    /// deriving them: a reordering that kept the set intact would still break
    /// every bytecode file already written.
    #[test]
    fn the_operand_bytes_are_the_ones_already_written_down() {
        for (index, op) in ALL.iter().enumerate() {
            assert_eq!(usize::from(op.tag()), index);
        }
        assert_eq!(MathOp::Sqrt.tag(), 0);
        assert_eq!(MathOp::Abs.tag(), 6);
        assert_eq!(MathOp::Exp.tag(), 7);
    }

    /// A square root a program cannot get wrong is the point of having one.
    ///
    /// The foundation's eight-iteration Newton `sqrtApprox` was accurate to
    /// about six digits; this is exact to the last bit.
    #[test]
    fn a_square_root_is_exact_rather_than_approximated() {
        assert!((MathOp::Sqrt.apply(&[2.0]) - std::f64::consts::SQRT_2).abs() < f64::EPSILON);
        assert_eq!(MathOp::Sqrt.apply(&[144.0]), 12.0);
    }

    #[test]
    fn the_trigonometric_operations_agree_with_the_unit_circle() {
        let pi = std::f64::consts::PI;
        assert!(MathOp::Sin.apply(&[0.0]).abs() < 1e-12);
        assert!((MathOp::Cos.apply(&[0.0]) - 1.0).abs() < 1e-12);
        assert!((MathOp::Sin.apply(&[pi / 2.0]) - 1.0).abs() < 1e-12);
        assert!(MathOp::Tan.apply(&[0.0]).abs() < 1e-12);
    }

    #[test]
    fn rounding_and_magnitude_answer_for_negatives_too() {
        assert_eq!(MathOp::Floor.apply(&[-1.5]), -2.0);
        assert_eq!(MathOp::Ceil.apply(&[-1.5]), -1.0);
        assert_eq!(MathOp::Abs.apply(&[-1.5]), 1.5);
        // Away from zero, which is where `round` and `trunc` disagree and where
        // a cast to `Int` would silently give the second answer for both.
        assert_eq!(MathOp::Round.apply(&[-1.5]), -2.0);
        assert_eq!(MathOp::Trunc.apply(&[-1.5]), -1.0);
        assert_eq!(MathOp::Round.apply(&[1.5]), 2.0);
    }

    /// The exponential and its inverse, at the points where a series expansion
    /// written in Kira would have started to drift.
    #[test]
    fn the_exponential_and_the_logarithm_invert_each_other() {
        assert!((MathOp::Exp.apply(&[1.0]) - std::f64::consts::E).abs() < 1e-15);
        assert!(MathOp::Log.apply(&[1.0]).abs() < 1e-15);
        assert!((MathOp::Log.apply(&[std::f64::consts::E]) - 1.0).abs() < 1e-15);
        assert!((MathOp::Exp.apply(&[MathOp::Log.apply(&[37.0])]) - 37.0).abs() < 1e-12);
        assert_eq!(MathOp::Log2.apply(&[1024.0]), 10.0);
        assert_eq!(MathOp::Log10.apply(&[1000.0]), 3.0);
        assert_eq!(MathOp::Exp2.apply(&[10.0]), 1024.0);
    }

    /// The scroll deceleration a motion library writes — `rate` to the power of
    /// a millisecond count — is the reason `pow` is a primitive rather than a
    /// loop, and it is not the same as `exp2` on a scaled exponent.
    #[test]
    fn the_binary_operations_read_both_of_their_operands() {
        assert_eq!(MathOp::Pow.apply(&[2.0, 10.0]), 1024.0);
        assert!((MathOp::Pow.apply(&[0.998, 500.0]) - 0.367_44).abs() < 1e-4);
        assert_eq!(MathOp::Min.apply(&[1.0, 2.0]), 1.0);
        assert_eq!(MathOp::Max.apply(&[1.0, 2.0]), 2.0);
        assert_eq!(MathOp::Hypot.apply(&[3.0, 4.0]), 5.0);
        assert_eq!(MathOp::CopySign.apply(&[1.5, -0.0]), -1.5);
        assert_eq!(MathOp::Fmod.apply(&[7.0, 4.0]), 3.0);
        assert_eq!(MathOp::Fmod.apply(&[-7.0, 4.0]), -3.0);
    }

    /// `atan2` takes its quadrant from both signs, which is the whole reason it
    /// exists beside `atan`.
    #[test]
    fn the_arc_tangent_of_two_operands_keeps_its_quadrant() {
        let pi = std::f64::consts::PI;
        assert!((MathOp::Atan2.apply(&[1.0, 1.0]) - pi / 4.0).abs() < 1e-15);
        assert!((MathOp::Atan2.apply(&[1.0, -1.0]) - 3.0 * pi / 4.0).abs() < 1e-15);
        assert!((MathOp::Atan.apply(&[1.0]) - pi / 4.0).abs() < 1e-15);
    }

    /// Arity is the operation's, and the two-operand ones are exactly the ones
    /// a caller has to push a second value for.
    #[test]
    fn only_the_two_operand_operations_ask_for_two() {
        for op in ALL {
            let expected = matches!(
                op,
                MathOp::Pow
                    | MathOp::Atan2
                    | MathOp::Min
                    | MathOp::Max
                    | MathOp::Hypot
                    | MathOp::CopySign
                    | MathOp::Fmod
            );
            assert_eq!(op.argument_count(), if expected { 2 } else { 1 });
        }
    }

    /// Engines size their operand buffers by `MAX_ARGUMENTS`, so an operation
    /// that outgrew it would overrun them.
    #[test]
    fn no_operation_takes_more_operands_than_an_engine_makes_room_for() {
        for op in ALL {
            assert!(op.argument_count() >= 1);
            assert!(op.argument_count() <= MathOp::MAX_ARGUMENTS);
        }
    }

    #[test]
    fn the_hyperbolic_operations_answer_at_zero_and_at_one() {
        assert!(MathOp::Sinh.apply(&[0.0]).abs() < 1e-15);
        assert!((MathOp::Cosh.apply(&[0.0]) - 1.0).abs() < 1e-15);
        assert!(MathOp::Tanh.apply(&[0.0]).abs() < 1e-15);
        assert!((MathOp::Tanh.apply(&[1.0]) - 0.761_594_155_955_764_9).abs() < 1e-15);
    }

    #[test]
    fn the_inverse_trigonometry_answers_within_its_domain() {
        let pi = std::f64::consts::PI;
        assert!((MathOp::Asin.apply(&[1.0]) - pi / 2.0).abs() < 1e-15);
        assert!(MathOp::Acos.apply(&[1.0]).abs() < 1e-15);
        assert!(MathOp::Asin.apply(&[2.0]).is_nan());
    }
}
