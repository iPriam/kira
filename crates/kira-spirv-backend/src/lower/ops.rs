//! The opcode each operator and each builtin takes.
//!
//! Tables rather than code because that is what they are, and because the
//! choice they make is invisible when it is wrong: `min` over unsigned integers
//! emitted as `min` over signed ones compiles, runs, and compares the wrong
//! bits.

use kira_ksl_semantics::model::{BinaryOp, BuiltinFn};
use kira_shader_model::{ScalarType, Type};

use crate::spec::{glsl, op};

/// The low 32 bits of `value`.
///
/// Total by construction: the mask leaves a value `u32` always holds, so this
/// narrows without a conversion that could fail.
pub(crate) fn low_word(value: u64) -> u32 {
    u32::try_from(value & 0xFFFF_FFFF).unwrap_or(0)
}

/// The scalar a type is made of, when it is made of one.
pub(crate) fn element_scalar(ty: &Type) -> Option<ScalarType> {
    match ty {
        Type::Scalar(scalar) => Some(*scalar),
        Type::Vector(vector) => Some(vector.scalar),
        Type::Matrix(_) => Some(ScalarType::Float),
        _ => None,
    }
}

/// The opcode one operator takes over one scalar kind.
pub(crate) fn opcode_for(operator: BinaryOp, scalar: ScalarType) -> u16 {
    let float = scalar == ScalarType::Float;
    let signed = scalar == ScalarType::Int;
    match operator {
        BinaryOp::Add => {
            if float {
                op::F_ADD
            } else {
                op::I_ADD
            }
        }
        BinaryOp::Sub => {
            if float {
                op::F_SUB
            } else {
                op::I_SUB
            }
        }
        BinaryOp::Mul => {
            if float {
                op::F_MUL
            } else {
                op::I_MUL
            }
        }
        BinaryOp::Div => {
            if float {
                op::F_DIV
            } else if signed {
                op::S_DIV
            } else {
                op::U_DIV
            }
        }
        BinaryOp::Rem => {
            if float {
                op::F_REM
            } else if signed {
                op::S_REM
            } else {
                op::U_MOD
            }
        }
        BinaryOp::Eq => {
            if float {
                op::F_ORD_EQUAL
            } else {
                op::I_EQUAL
            }
        }
        BinaryOp::Ne => {
            if float {
                op::F_ORD_NOT_EQUAL
            } else {
                op::I_NOT_EQUAL
            }
        }
        BinaryOp::Lt => {
            if float {
                op::F_ORD_LESS_THAN
            } else if signed {
                op::S_LESS_THAN
            } else {
                op::U_LESS_THAN
            }
        }
        BinaryOp::Le => {
            if float {
                op::F_ORD_LESS_THAN_EQUAL
            } else if signed {
                op::S_LESS_THAN_EQUAL
            } else {
                op::U_LESS_THAN_EQUAL
            }
        }
        BinaryOp::Gt => {
            if float {
                op::F_ORD_GREATER_THAN
            } else if signed {
                op::S_GREATER_THAN
            } else {
                op::U_GREATER_THAN
            }
        }
        BinaryOp::Ge => {
            if float {
                op::F_ORD_GREATER_THAN_EQUAL
            } else if signed {
                op::S_GREATER_THAN_EQUAL
            } else {
                op::U_GREATER_THAN_EQUAL
            }
        }
        BinaryOp::And => op::LOGICAL_AND,
        BinaryOp::Or => op::LOGICAL_OR,
        BinaryOp::BitAnd => op::BITWISE_AND,
        BinaryOp::BitOr => op::BITWISE_OR,
        BinaryOp::BitXor => op::BITWISE_XOR,
        BinaryOp::Shl => op::SHIFT_LEFT_LOGICAL,
        BinaryOp::Shr => {
            if signed {
                op::SHIFT_RIGHT_ARITHMETIC
            } else {
                op::SHIFT_RIGHT_LOGICAL
            }
        }
    }
}

/// The `GLSL.std.450` instruction a builtin takes over one scalar kind.
///
/// Several of them are three instructions rather than one — `min` over floats,
/// over signed integers, and over unsigned ones are `FMin`, `SMin` and `UMin`,
/// and picking the wrong one compares the wrong bits.
pub(crate) fn extended_instruction(which: BuiltinFn, scalar: ScalarType) -> Option<u32> {
    let float = scalar == ScalarType::Float;
    let signed = scalar == ScalarType::Int;
    let by_kind = |float_form: u32, signed_form: u32, unsigned_form: u32| {
        if float {
            float_form
        } else if signed {
            signed_form
        } else {
            unsigned_form
        }
    };
    Some(match which {
        // Not extended at all — the caller emits `OpDot` and only asks here so
        // an unsupported builtin still answers `None`.
        BuiltinFn::Dot => 0,
        BuiltinFn::Cross => glsl::CROSS,
        BuiltinFn::Normalize => glsl::NORMALIZE,
        BuiltinFn::Length => glsl::LENGTH,
        BuiltinFn::Abs => {
            if float {
                glsl::F_ABS
            } else {
                glsl::S_ABS
            }
        }
        BuiltinFn::Floor => glsl::FLOOR,
        BuiltinFn::Ceil => glsl::CEIL,
        BuiltinFn::Fract => glsl::FRACT,
        BuiltinFn::Min => by_kind(glsl::F_MIN, glsl::S_MIN, glsl::U_MIN),
        BuiltinFn::Max => by_kind(glsl::F_MAX, glsl::S_MAX, glsl::U_MAX),
        BuiltinFn::Clamp => by_kind(glsl::F_CLAMP, glsl::S_CLAMP, glsl::U_CLAMP),
        BuiltinFn::Mix => glsl::F_MIX,
        BuiltinFn::Step => glsl::STEP,
        BuiltinFn::Smoothstep => glsl::SMOOTH_STEP,
        BuiltinFn::Pow => glsl::POW,
        BuiltinFn::Sqrt => glsl::SQRT,
        BuiltinFn::Sin => glsl::SIN,
        BuiltinFn::Cos => glsl::COS,
        BuiltinFn::Tan => glsl::TAN,
        BuiltinFn::Atan2 => glsl::ATAN2,
        BuiltinFn::Exp => glsl::EXP,
        BuiltinFn::Log => glsl::LOG,
        BuiltinFn::Mul
        | BuiltinFn::Sample
        | BuiltinFn::Load
        | BuiltinFn::Store
        | BuiltinFn::AtomicAdd => return None,
    })
}
