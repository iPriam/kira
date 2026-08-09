//! How a numeric type was *spelled*, and what that spelling decides.
//!
//! Kira has one integer runtime representation (64-bit two's complement) and
//! one float representation (64-bit IEEE-754). The fixed-width names — `I8`
//! through `I32`, `U8` through `U64`, `F32` — do not introduce new
//! representations. They are **spellings** carried alongside the kind, and they
//! decide exactly two things:
//!
//! 1. **Type distinctness.** Two *named* widths must match exactly, so `U8` is
//!    not assignable to `U32`. Bare `Int` and bare `Float` are wildcards that
//!    match any width in their kind — which is what lets `let x: U8 = 5` work
//!    without an implicit-conversion rule, since an integer literal is spelled
//!    [`IntSpelling::Plain`]. See [`super::Type::assignable_to`].
//! 2. **Signedness of division, remainder, and ordering.** A `U`-prefixed
//!    spelling selects unsigned `/`, `%`, `<`, `<=`, `>`, `>=`. See
//!    [`IntSpelling::is_unsigned`].
//!
//! Nothing else depends on the width. Addition, subtraction, and multiplication
//! wrap at 64 bits for **every** spelling: assigning `250 + 10` to a `U8` yields
//! `260`, not `4`. That is not an omission — narrowing arithmetic to the written
//! width is behavior the language does not define, so this port declines to
//! invent it.

/// How an integer type was spelled in source.
///
/// The runtime representation is 64-bit two's complement for every variant;
/// this records the written name only. See the module docs for the two things
/// it decides.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntSpelling {
    /// Bare `Int`, and the type of every integer literal.
    ///
    /// Acts as a wildcard in [`super::Type::assignable_to`]: it is assignable
    /// to and from every other spelling. Signed.
    Plain,
    /// `I8`.
    I8,
    /// `I16`.
    I16,
    /// `I32`.
    I32,
    /// `U8`.
    U8,
    /// `U16`.
    U16,
    /// `U32`.
    U32,
    /// `U64`.
    U64,
}

impl IntSpelling {
    /// Whether this spelling selects unsigned division, remainder, and
    /// ordering.
    ///
    /// True for exactly the `U`-prefixed names. Bare `Int` and `I8`..`I32` are
    /// signed. Equality is *not* affected — two 64-bit patterns compare equal
    /// under either signedness — so only `/`, `%`, and the four ordering
    /// comparisons consult this.
    pub fn is_unsigned(self) -> bool {
        matches!(
            self,
            IntSpelling::U8 | IntSpelling::U16 | IntSpelling::U32 | IntSpelling::U64
        )
    }

    /// The canonical spelling, for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            IntSpelling::Plain => "Int",
            IntSpelling::I8 => "I8",
            IntSpelling::I16 => "I16",
            IntSpelling::I32 => "I32",
            IntSpelling::U8 => "U8",
            IntSpelling::U16 => "U16",
            IntSpelling::U32 => "U32",
            IntSpelling::U64 => "U64",
        }
    }

    /// Resolves a written fixed-width integer name, or `None` when it is not
    /// one.
    ///
    /// Deliberately does **not** answer for `"Int"`: bare `Int` is resolved by
    /// [`super::Type::from_name`] alongside the other kinds, and routing it
    /// here as well would give one name two resolution paths.
    pub fn from_name(name: &str) -> Option<IntSpelling> {
        Some(match name {
            "I8" => IntSpelling::I8,
            "I16" => IntSpelling::I16,
            "I32" => IntSpelling::I32,
            "U8" => IntSpelling::U8,
            "U16" => IntSpelling::U16,
            "U32" => IntSpelling::U32,
            "U64" => IntSpelling::U64,
            _ => return None,
        })
    }
}

/// How a floating-point type was spelled in source.
///
/// The runtime representation is 64-bit IEEE-754 for every variant. Unlike
/// [`IntSpelling`], no operation's behavior depends on this: it exists purely
/// so `F32` and bare `Float` are distinct types to the checker.
///
/// There is no `F64`. `Float` *is* the 64-bit float — a second spelling for one
/// type bought nothing and cost every reader a moment deciding which to write.
/// The same is true of `Int`, which is why there is no `I64` either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatSpelling {
    /// Bare `Float`, and the type of every float literal.
    ///
    /// A wildcard in [`super::Type::assignable_to`], exactly as
    /// [`IntSpelling::Plain`] is.
    Plain,
    /// `F32`.
    F32,
}

impl FloatSpelling {
    /// The canonical spelling, for diagnostics.
    pub fn name(self) -> &'static str {
        match self {
            FloatSpelling::Plain => "Float",
            FloatSpelling::F32 => "F32",
        }
    }

    /// Resolves a written fixed-width float name, or `None` when it is not one.
    ///
    /// As with [`IntSpelling::from_name`], bare `Float` is not answered here.
    pub fn from_name(name: &str) -> Option<FloatSpelling> {
        Some(match name {
            "F32" => FloatSpelling::F32,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_u_prefixed_spellings_are_unsigned() {
        for signed in [
            IntSpelling::Plain,
            IntSpelling::I8,
            IntSpelling::I16,
            IntSpelling::I32,
        ] {
            assert!(!signed.is_unsigned(), "{signed:?} is signed");
        }
        for unsigned in [
            IntSpelling::U8,
            IntSpelling::U16,
            IntSpelling::U32,
            IntSpelling::U64,
        ] {
            assert!(unsigned.is_unsigned(), "{unsigned:?} is unsigned");
        }
    }

    #[test]
    fn every_fixed_width_name_round_trips() {
        for spelling in [
            IntSpelling::I8,
            IntSpelling::I16,
            IntSpelling::I32,
            IntSpelling::U8,
            IntSpelling::U16,
            IntSpelling::U32,
            IntSpelling::U64,
        ] {
            assert_eq!(IntSpelling::from_name(spelling.name()), Some(spelling));
        }
        // `F32` is the only fixed-width float spelling: `Float` is the
        // wildcard, and there is no `F64`.
        assert_eq!(
            FloatSpelling::from_name(FloatSpelling::F32.name()),
            Some(FloatSpelling::F32)
        );
    }

    #[test]
    fn the_plain_spellings_do_not_resolve_as_fixed_width_names() {
        // `Int` and `Float` are resolved by `Type::from_name`; giving them a
        // second resolution path here is how the two could drift apart.
        assert_eq!(IntSpelling::from_name("Int"), None);
        assert_eq!(FloatSpelling::from_name("Float"), None);
        // And the widths they replaced are gone outright: `Int` *is* the 64-bit
        // signed integer and `Float` the 64-bit float, so a second spelling for
        // either would be one type wearing two names.
        assert_eq!(IntSpelling::from_name("I64"), None);
        assert_eq!(FloatSpelling::from_name("F64"), None);
    }

    #[test]
    fn there_is_no_128_bit_or_char_spelling() {
        // The corpus census has no I128/U128 and no Char. Inventing one here
        // would be inventing language surface.
        for absent in ["I128", "U128", "Char", "USize", "ISize", "F16"] {
            assert_eq!(IntSpelling::from_name(absent), None);
            assert_eq!(FloatSpelling::from_name(absent), None);
        }
    }
}
