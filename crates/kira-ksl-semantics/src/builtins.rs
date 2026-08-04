//! The names KSL provides: scalar and vector types, and the builtin functions.
//!
//! Kept in one table so the parser stays ignorant of them — `Float4` and
//! `normalize` are an ordinary path and an ordinary call until they reach here.

use kira_shader_model::{MatrixType, SamplerKind, ScalarType, TextureDimension, Type, VectorType};

use crate::model::BuiltinFn;

/// The type `name` spells, when KSL provides one by that name.
#[must_use]
pub fn builtin_type(name: &str) -> Option<Type> {
    let scalar = |scalar| Some(Type::Scalar(scalar));
    let vector = |scalar, width| Some(Type::Vector(VectorType { scalar, width }));
    match name {
        "Bool" => scalar(ScalarType::Bool),
        "Int" => scalar(ScalarType::Int),
        "UInt" => scalar(ScalarType::Uint),
        "Float" => scalar(ScalarType::Float),

        "Bool2" => vector(ScalarType::Bool, 2),
        "Bool3" => vector(ScalarType::Bool, 3),
        "Bool4" => vector(ScalarType::Bool, 4),
        "Int2" => vector(ScalarType::Int, 2),
        "Int3" => vector(ScalarType::Int, 3),
        "Int4" => vector(ScalarType::Int, 4),
        "UInt2" => vector(ScalarType::Uint, 2),
        "UInt3" => vector(ScalarType::Uint, 3),
        "UInt4" => vector(ScalarType::Uint, 4),
        "Float2" => vector(ScalarType::Float, 2),
        "Float3" => vector(ScalarType::Float, 3),
        "Float4" => vector(ScalarType::Float, 4),

        "Float2x2" => Some(Type::Matrix(MatrixType {
            columns: 2,
            rows: 2,
        })),
        "Float3x3" => Some(Type::Matrix(MatrixType {
            columns: 3,
            rows: 3,
        })),
        "Float4x4" => Some(Type::Matrix(MatrixType {
            columns: 4,
            rows: 4,
        })),

        "Texture2d" => Some(Type::Texture(TextureDimension::Texture2d)),
        "TextureCube" => Some(Type::Texture(TextureDimension::TextureCube)),
        "Depth2d" => Some(Type::Texture(TextureDimension::Depth2d)),
        "Texture2dUint" => Some(Type::Texture(TextureDimension::Texture2dUint)),

        "Sampler" => Some(Type::Sampler(SamplerKind::Filtering)),
        "SamplerComparison" => Some(Type::Sampler(SamplerKind::Comparison)),

        "Void" => Some(Type::Void),
        _ => None,
    }
}

/// The builtin function `name` calls, when it calls one.
#[must_use]
pub fn builtin_fn(name: &str) -> Option<BuiltinFn> {
    Some(match name {
        "mul" => BuiltinFn::Mul,
        "dot" => BuiltinFn::Dot,
        "cross" => BuiltinFn::Cross,
        "normalize" => BuiltinFn::Normalize,
        "length" => BuiltinFn::Length,
        "abs" => BuiltinFn::Abs,
        "floor" => BuiltinFn::Floor,
        "ceil" => BuiltinFn::Ceil,
        "min" => BuiltinFn::Min,
        "max" => BuiltinFn::Max,
        "clamp" => BuiltinFn::Clamp,
        "mix" => BuiltinFn::Mix,
        "step" => BuiltinFn::Step,
        "smoothstep" => BuiltinFn::Smoothstep,
        "pow" => BuiltinFn::Pow,
        "sqrt" => BuiltinFn::Sqrt,
        "sin" => BuiltinFn::Sin,
        "cos" => BuiltinFn::Cos,
        "tan" => BuiltinFn::Tan,
        "atan2" => BuiltinFn::Atan2,
        "exp" => BuiltinFn::Exp,
        "log" => BuiltinFn::Log,
        "fract" => BuiltinFn::Fract,
        "sample" => BuiltinFn::Sample,
        "load" => BuiltinFn::Load,
        "store" => BuiltinFn::Store,
        "atomicAdd" => BuiltinFn::AtomicAdd,
        _ => return None,
    })
}

/// How many arguments `which` takes.
#[must_use]
pub fn arity(which: BuiltinFn) -> usize {
    match which {
        BuiltinFn::Normalize
        | BuiltinFn::Length
        | BuiltinFn::Abs
        | BuiltinFn::Floor
        | BuiltinFn::Ceil
        | BuiltinFn::Sqrt
        | BuiltinFn::Sin
        | BuiltinFn::Cos
        | BuiltinFn::Tan
        | BuiltinFn::Exp
        | BuiltinFn::Log
        | BuiltinFn::Fract => 1,
        BuiltinFn::Mul
        | BuiltinFn::Dot
        | BuiltinFn::Cross
        | BuiltinFn::Min
        | BuiltinFn::Max
        | BuiltinFn::Step
        | BuiltinFn::Pow
        | BuiltinFn::Atan2
        | BuiltinFn::Load => 2,
        BuiltinFn::Clamp
        | BuiltinFn::Mix
        | BuiltinFn::Smoothstep
        | BuiltinFn::Sample
        | BuiltinFn::Store
        | BuiltinFn::AtomicAdd => 3,
    }
}

/// The builtin stage value `word` names, when it names one.
#[must_use]
pub fn builtin_value(word: &str) -> Option<kira_shader_model::Builtin> {
    use kira_shader_model::Builtin;
    Some(match word {
        "position" => Builtin::Position,
        "vertex_index" => Builtin::VertexIndex,
        "instance_index" => Builtin::InstanceIndex,
        "front_facing" => Builtin::FrontFacing,
        "frag_coord" => Builtin::FragCoord,
        "thread_id" => Builtin::ThreadId,
        "local_id" => Builtin::LocalId,
        "group_id" => Builtin::GroupId,
        "local_index" => Builtin::LocalIndex,
        _ => return None,
    })
}

/// The interpolation qualifier `word` names, when it names one.
#[must_use]
pub fn interpolation(word: &str) -> Option<kira_shader_model::Interpolation> {
    use kira_shader_model::Interpolation;
    Some(match word {
        "perspective" => Interpolation::Perspective,
        "linear" => Interpolation::Linear,
        "flat" => Interpolation::Flat,
        _ => return None,
    })
}

/// The component index `letter` selects, when it selects one.
#[must_use]
pub fn swizzle_component(letter: char) -> Option<u8> {
    Some(match letter {
        'x' | 'r' => 0,
        'y' | 'g' => 1,
        'z' | 'b' => 2,
        'w' | 'a' => 3,
        _ => return None,
    })
}

/// The component indices `name` selects, when every letter selects one.
#[must_use]
pub fn swizzle(name: &str) -> Option<Vec<u8>> {
    if name.is_empty() || name.len() > 4 {
        return None;
    }
    name.chars().map(swizzle_component).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_vector_names_the_corpus_writes_all_resolve() {
        for name in ["Float2", "Float3", "Float4", "UInt3", "Float4x4"] {
            assert!(builtin_type(name).is_some(), "{name}");
        }
    }

    #[test]
    fn a_struct_name_is_not_a_builtin_type() {
        assert_eq!(builtin_type("VertexIn"), None);
    }

    #[test]
    fn a_swizzle_needs_every_letter_to_be_a_component() {
        assert_eq!(swizzle("x"), Some(vec![0]));
        assert_eq!(swizzle("xyz"), Some(vec![0, 1, 2]));
        assert_eq!(swizzle("wzyx"), Some(vec![3, 2, 1, 0]));
        assert_eq!(swizzle("position"), None);
        assert_eq!(swizzle(""), None);
    }

    #[test]
    fn every_builtin_the_corpus_calls_is_known() {
        for name in [
            "mul",
            "dot",
            "normalize",
            "sample",
            "load",
            "atomicAdd",
            "pow",
            "smoothstep",
            "sin",
            "floor",
            "atan2",
            "length",
        ] {
            assert!(builtin_fn(name).is_some(), "{name}");
        }
    }
}
