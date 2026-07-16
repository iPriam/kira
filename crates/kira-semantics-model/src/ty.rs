//! The v0 type lattice.
//!
//! The subset is monomorphic and closed: four value types plus `Void` and an
//! `Error` type that absorbs mismatches so one type error does not cascade
//! into a storm of follow-on diagnostics.

/// A resolved Kira type in the v0 subset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Type {
    /// The 64-bit signed integer type (`Int`).
    Int,
    /// The 64-bit floating-point type (`Float`).
    Float,
    /// The boolean type (`Bool`).
    Bool,
    /// The heap string type (`String`).
    String,
    /// The unit type of statements and value-less returns (`Void`).
    Void,
    /// The absorbing error type; assignable to and from anything.
    Error,
}

impl Type {
    /// Resolves a written type name to a v0 type, or `None` when unknown.
    pub fn from_name(name: &str) -> Option<Type> {
        Some(match name {
            "Int" => Type::Int,
            "Float" => Type::Float,
            "Bool" => Type::Bool,
            "String" => Type::String,
            "Void" => Type::Void,
            _ => return None,
        })
    }

    /// The canonical spelling of this type, for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            Type::Int => "Int",
            Type::Float => "Float",
            Type::Bool => "Bool",
            Type::String => "String",
            Type::Void => "Void",
            Type::Error => "<error>",
        }
    }

    /// Whether a value of `self` may be used where `target` is expected.
    ///
    /// v0 requires exact matches (no implicit `Int`->`Float` widening); the
    /// `Error` type is compatible in both directions to stop cascades.
    pub fn assignable_to(self, target: Type) -> bool {
        self == Type::Error || target == Type::Error || self == target
    }

    /// Whether this is one of the numeric types (`Int` or `Float`).
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// Whether values of this type can be passed to the `print` builtin.
    pub fn is_printable(self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Bool | Type::String)
    }
}
