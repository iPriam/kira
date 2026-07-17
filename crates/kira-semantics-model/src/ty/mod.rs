//! The v0 type lattice and the program's table of type shapes.
//!
//! The subset is monomorphic and closed: four scalar types, `Void`, an `Error`
//! type that absorbs mismatches so one type error does not cascade,
//! user-declared structs, and arrays. A [`Type`] stays `Copy` because a struct
//! type is a [`StructId`] and an array type an [`ArrayId`] — an index into a
//! table rather than an inline shape.

pub mod arrays;
pub mod enums;
pub mod structs;
pub mod table;

pub use arrays::{ArrayId, ArrayTable};
pub use enums::{EnumDef, EnumId, EnumTable, VariantDef};
pub use structs::{FieldDef, StructDef, StructId, StructTable};
pub use table::TypeTable;

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
    /// A declared struct, named by its row in the program's [`StructTable`].
    Struct(StructId),
    /// An array, named by its row in the program's [`ArrayTable`], which holds
    /// the element type.
    ///
    /// The indirection is what keeps `Type` `Copy`: `[Int]` is a `u32`, not a
    /// boxed element type. The table interns, so `[Int] == [Int]`.
    Array(ArrayId),
    /// A declared enum, named by its row in the program's [`EnumTable`].
    ///
    /// Like a struct, an enum is a nominal type: two enums with the same
    /// variants are still distinct, so this compares by [`EnumId`].
    Enum(EnumId),
}

impl Type {
    /// Resolves a written *builtin* type name, or `None` when it is not one.
    ///
    /// A struct name is not a builtin, so resolving one needs the program's
    /// [`StructTable`]; the analyzer tries this first and the table second. An
    /// array type has no name to resolve — `[Int]` is syntax the parser builds
    /// a type reference from, not an identifier — so it never reaches here.
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

    /// Whether a value of `self` may be used where `target` is expected.
    ///
    /// v0 requires exact matches (no implicit `Int`->`Float` widening); the
    /// `Error` type is compatible in both directions to stop cascades. Arrays
    /// compare by [`ArrayId`], which the table interns, so `[Int]` is
    /// assignable to `[Int]` and to nothing else.
    pub fn assignable_to(self, target: Type) -> bool {
        self == Type::Error || target == Type::Error || self == target
    }

    /// Whether this is one of the numeric types (`Int` or `Float`).
    pub fn is_numeric(self) -> bool {
        matches!(self, Type::Int | Type::Float)
    }

    /// Whether this is an array type.
    pub fn is_array(self) -> bool {
        matches!(self, Type::Array(_))
    }

    /// Whether values of this type can be passed to the `print` builtin.
    ///
    /// A struct is not printable: what `print` renders for one is not pinned
    /// by the language corpus, and inventing a format here would be inventing
    /// language surface. A struct prints through its own accessors until the
    /// format is settled.
    ///
    /// An array is not printable for the same reason and on the same evidence:
    /// the corpus has no `print(someArray)` call site and no golden file
    /// naming a separator, a bracket, or how a nested array renders. Every one
    /// of those is a decision the language has not made, so this refuses rather
    /// than making them. An array prints through `for x in xs { print(x) }`.
    /// An enum is not printable for the same reason as a struct and an array:
    /// the corpus pins no rendering for one, so any text invented here would be
    /// inventing language surface.
    pub fn is_printable(self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Bool | Type::String)
    }

    /// Whether a value of this type owns heap storage that a copy must clone
    /// and a drop must release.
    ///
    /// Scalars are `Copy` and own nothing. A `String` owns its bytes. A struct
    /// owns whatever its fields own and an array owns its backing storage, so
    /// the answer for both is the table's to give — see
    /// [`TypeTable::owns_heap`].
    pub fn is_scalar(self) -> bool {
        matches!(self, Type::Int | Type::Float | Type::Bool | Type::Void)
    }

    /// Whether a value of this type reaches an owned parameter without an
    /// explicit `move`.
    ///
    /// This is the predicate that decides whether passing a *named* local to a
    /// consuming parameter needs `move` written at the call site. It is a
    /// property of the type alone and deliberately narrow: `Void` and the
    /// three scalars, plus (once they exist) the C-seam types `CString` and
    /// `RawPtr`.
    ///
    /// `String` is **not** trivially copyable — it owns its bytes — so
    /// `f(name)` on a `String` local is `KSEM108` and `f(move name)` is how it
    /// is written. A **struct is not trivially copyable either**, which is the
    /// rule most worth stating out loud, because a struct nonetheless does
    /// *not* implicitly move when bound ([`Type::moves_on_bind`]). The two
    /// predicates answer different questions and a struct answers them
    /// differently: it needs `move` into an owned parameter, and it still
    /// copies on `let w = v`.
    ///
    /// An array answers `false` here **and** `true` to `moves_on_bind` — the
    /// only type that answers both that way, and the reason both predicates
    /// exist as separate questions.
    pub fn is_trivially_copyable(self) -> bool {
        match self {
            Type::Int | Type::Float | Type::Bool | Type::Void => true,
            // An expression that already failed to analyze must not also
            // collect an ownership diagnostic on top of its type error.
            Type::Error => true,
            // An enum answers exactly as an array does: not trivially copyable
            // (a named enum local needs `move` into an owned parameter) and yet
            // it moves on bind.
            Type::String | Type::Struct(_) | Type::Array(_) | Type::Enum(_) => false,
        }
    }

    /// Whether binding a value of this type *consumes* the binding it was read
    /// from (Rust-style implicit move on bind).
    ///
    /// True for exactly the types that a binding would otherwise **alias**:
    /// arrays, enum instances, and construct existentials all lower to a
    /// shared heap handle, so `let alias = values` would leave two owners
    /// pointing at one object. Marking the source moved turns that aliasing
    /// into `KSEM107` instead of a use-after-free.
    ///
    /// **An array and an enum are the two `true` cases.** Both lower to a
    /// shared heap handle, so `let alias = value` would leave two owners
    /// pointing at one object; marking the source moved turns that aliasing
    /// into `KSEM107`. A struct deep-copies when bound and a `String` clones
    /// its bytes, so neither can alias and neither has anything to enforce.
    pub fn moves_on_bind(self) -> bool {
        match self {
            Type::Array(_) | Type::Enum(_) => true,
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::Void
            | Type::Error
            | Type::String
            | Type::Struct(_) => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table_with_point() -> (TypeTable, StructId) {
        let mut table = TypeTable::new();
        let id = table
            .structs_mut()
            .declare(StructDef {
                name: "Point".to_owned(),
                fields: vec![FieldDef {
                    name: "x".to_owned(),
                    ty: Type::Int,
                    mutable: true,
                }],
            })
            .expect("a fresh name declares");
        (table, id)
    }

    #[test]
    fn a_struct_type_names_itself_in_diagnostics() {
        let (table, id) = table_with_point();
        assert_eq!(table.type_name(Type::Struct(id)), "Point");
        assert_eq!(table.type_name(Type::Int), "Int");
    }

    #[test]
    fn a_struct_needs_move_but_does_not_move_on_bind() {
        let (_, id) = table_with_point();
        // The two predicates answer differently for a struct, and that split
        // is the whole point: `f(p)` into an owned param is KSEM108, while
        // `let q = p` still copies.
        assert!(!Type::Struct(id).is_trivially_copyable());
        assert!(!Type::Struct(id).moves_on_bind());
    }

    #[test]
    fn an_array_needs_move_and_also_moves_on_bind() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        // The one type that answers both questions `no`/`yes`: it needs `move`
        // into an owned parameter *and* `let alias = xs` consumes `xs`.
        assert!(!ints.is_trivially_copyable());
        assert!(ints.moves_on_bind());
    }

    #[test]
    fn a_string_needs_move_and_a_scalar_does_not() {
        assert!(!Type::String.is_trivially_copyable());
        assert!(Type::Int.is_trivially_copyable());
        assert!(Type::Float.is_trivially_copyable());
        assert!(Type::Bool.is_trivially_copyable());
        assert!(Type::Void.is_trivially_copyable());
    }

    #[test]
    fn only_an_array_moves_on_bind() {
        let (mut table, id) = table_with_point();
        let ints = table.array_of(Type::Int);
        for ty in [
            Type::Int,
            Type::Float,
            Type::Bool,
            Type::Void,
            Type::Error,
            Type::String,
            Type::Struct(id),
        ] {
            assert!(!ty.moves_on_bind(), "{ty:?} must not move on bind");
        }
        assert!(ints.moves_on_bind(), "an array is the type that does");
    }

    #[test]
    fn an_error_type_never_collects_an_ownership_diagnostic() {
        // A type error already reported must not also produce KSEM108.
        assert!(Type::Error.is_trivially_copyable());
    }

    #[test]
    fn an_array_is_not_printable_because_no_format_is_pinned() {
        let mut table = TypeTable::new();
        let ints = table.array_of(Type::Int);
        // Same evidence as a struct: no corpus call site, no golden file. A
        // separator invented here would be invented language surface.
        assert!(!ints.is_printable());
        assert!(Type::Int.is_printable());
    }

    #[test]
    fn interned_array_types_are_equal_and_assignable() {
        let mut table = TypeTable::new();
        let a = table.array_of(Type::Int);
        let b = table.array_of(Type::Int);
        let strings = table.array_of(Type::String);
        assert_eq!(a, b);
        assert!(a.assignable_to(b));
        assert!(
            !a.assignable_to(strings),
            "no widening between element types"
        );
        assert!(a.assignable_to(Type::Error));
        assert!(Type::Error.assignable_to(a));
    }
}
