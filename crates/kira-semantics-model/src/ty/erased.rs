//! The type identity a value carries once it has been erased into `Any`.
//!
//! # Why erasure needs an identity at all
//!
//! `Any` is opaque in the read direction, so for most of its life a boxed value
//! only has to be *carried* — and carrying needs no more than the coarse kind
//! each backend already keeps to free the box. Comparing two of them needs
//! more. Native code holds an aggregate as untyped bytes plus generated
//! clone/free leaves, so reading a `Rect`'s bytes through a `Point`'s layout is
//! undefined behavior rather than a wrong answer. Something in the box has to
//! say which Kira type wrote it.
//!
//! The VM has the opposite shortfall and the same conclusion. Its heap objects
//! are structural — a struct is a tuple of values, deliberately, with no type
//! table reaching the runtime — so on its own it would call `Point(1, 2)` and
//! `Rect(1, 2)` equal where native cannot. One identity written at the erasure
//! site is what lets both engines answer alike.
//!
//! # Why this is a pure function and not a table
//!
//! [`Type`] is `Copy` and flat: a struct, an array, and an enum are each an
//! interned row index, not a boxed tree. So an id is computable from the type
//! alone, and two backends reading the same [`Type`] compute the same number
//! without sharing a table, agreeing on a traversal order, or interning in
//! lockstep. A table would have to be built once and threaded to both; this
//! cannot drift.
//!
//! # What counts as one type
//!
//! Integer spellings collapse to one id, and float spellings likewise. This
//! follows the rule `==` already applies to unerased operands, where numerics
//! unify before they compare (`kira-semantics`' `equality`): `Int` is a
//! wildcard assignable to and from every sized spelling, so an erased `I32` and
//! an erased `Int` holding the same bits are the same value by every other
//! measure the language offers. Structs, arrays, and enums are nominal and keep
//! their row index, because the language already treats them that way.

use super::Type;

/// Which family of types an [`ErasedTypeId`] names, in its high word.
///
/// Values are a wire contract: the id is written into the erasure box on native
/// and into the `Erase` instruction's immediate on the VM, so these are
/// **append-only** — a new family takes the next free number and the existing
/// ones never move.
mod kind {
    /// Every integer spelling.
    pub(super) const INT: u64 = 0;
    /// Every float spelling.
    pub(super) const FLOAT: u64 = 1;
    /// `Bool`.
    pub(super) const BOOL: u64 = 2;
    /// `String`.
    pub(super) const STRING: u64 = 3;
    /// `RawPtr`.
    pub(super) const RAW_PTR: u64 = 4;
    /// A declared struct, indexed by its `StructId`.
    pub(super) const STRUCT: u64 = 5;
    /// An array, indexed by its `ArrayId`.
    pub(super) const ARRAY: u64 = 6;
    /// A declared enum, indexed by its `EnumId`.
    pub(super) const ENUM: u64 = 7;
}

/// The identity an erased value carries, so two of them can be compared.
///
/// A family in the high 32 bits and a row index in the low 32. Two types are
/// the same erased type exactly when their ids are equal, which is what makes
/// this comparable as one integer on both engines and across the C seam.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ErasedTypeId(u64);

impl ErasedTypeId {
    /// The id of `ty`, or `None` for a type that never erases.
    ///
    /// `Void`, `Error`, `Cell`, `Task`, `NativeState`, and `Any` itself are the
    /// types with no id: none of them is assignable to `Any`
    /// ([`Type::assignable_to`]), so reaching here with one means analysis let
    /// through something it refuses, and the caller reports that rather than
    /// inventing a number.
    pub fn of(ty: Type) -> Option<Self> {
        let raw = match ty {
            // Spellings collapse: see the module header.
            Type::Int(_) => kind::INT << 32,
            Type::Float(_) => kind::FLOAT << 32,
            Type::Bool => kind::BOOL << 32,
            Type::String => kind::STRING << 32,
            Type::RawPtr => kind::RAW_PTR << 32,
            Type::Struct(id) => (kind::STRUCT << 32) | u64::from(id.index()),
            Type::Array(id) => (kind::ARRAY << 32) | u64::from(id.index()),
            Type::Enum(id) => (kind::ENUM << 32) | u64::from(id.index()),
            Type::Void
            | Type::Error
            | Type::CString
            | Type::Cell(_)
            | Type::Task(_)
            | Type::NativeState(_)
            | Type::Any => return None,
        };
        Some(Self(raw))
    }

    /// This id as the raw word the backends carry.
    pub fn as_u64(self) -> u64 {
        self.0
    }

    /// This id as the signed word the native seam passes.
    ///
    /// The runtime only ever compares two of these for equality, so the
    /// reinterpretation is free of meaning — it exists because the seam speaks
    /// `i64` and nothing else about the number matters.
    pub fn as_i64(self) -> i64 {
        self.0 as i64
    }

    /// Rebuilds an id from the raw word a backend carried.
    pub fn from_u64(raw: u64) -> Self {
        Self(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ty::table::TypeTable;
    use crate::ty::{FloatSpelling, IntSpelling};

    #[test]
    fn every_integer_spelling_is_one_erased_type() {
        let plain = ErasedTypeId::of(Type::Int(IntSpelling::Plain));
        assert_eq!(plain, ErasedTypeId::of(Type::Int(IntSpelling::I32)));
        assert_eq!(plain, ErasedTypeId::of(Type::Int(IntSpelling::U8)));
    }

    #[test]
    fn every_float_spelling_is_one_erased_type() {
        let plain = ErasedTypeId::of(Type::Float(FloatSpelling::Plain));
        assert_eq!(plain, ErasedTypeId::of(Type::Float(FloatSpelling::F32)));
    }

    #[test]
    fn the_scalar_families_are_distinct() {
        let ids = [
            ErasedTypeId::of(Type::Int(IntSpelling::Plain)),
            ErasedTypeId::of(Type::Float(FloatSpelling::Plain)),
            ErasedTypeId::of(Type::Bool),
            ErasedTypeId::of(Type::String),
            ErasedTypeId::of(Type::RawPtr),
        ];
        for (index, one) in ids.iter().enumerate() {
            assert!(one.is_some());
            for other in &ids[index + 1..] {
                assert_ne!(one, other);
            }
        }
    }

    /// The case that forced an identity into the box: two nominal types with
    /// the same shape must not share an id.
    #[test]
    fn two_arrays_of_different_elements_are_different_erased_types() {
        let mut types = TypeTable::new();
        let ints = types.array_of(Type::Int(IntSpelling::Plain));
        let bools = types.array_of(Type::Bool);
        assert_ne!(ErasedTypeId::of(ints), ErasedTypeId::of(bools));
        // Interning means the same element type gives the same id back.
        let again = types.array_of(Type::Int(IntSpelling::Plain));
        assert_eq!(ErasedTypeId::of(ints), ErasedTypeId::of(again));
    }

    #[test]
    fn a_type_that_never_erases_has_no_id() {
        assert_eq!(ErasedTypeId::of(Type::Void), None);
        assert_eq!(ErasedTypeId::of(Type::Any), None);
        assert_eq!(ErasedTypeId::of(Type::Error), None);
    }

    /// The encoding is a wire contract on both engines; pin the words.
    #[test]
    fn the_family_words_are_pinned() {
        let int = ErasedTypeId::of(Type::Int(IntSpelling::Plain));
        assert_eq!(int.map(ErasedTypeId::as_u64), Some(0));
        assert_eq!(
            ErasedTypeId::of(Type::Bool).map(ErasedTypeId::as_u64),
            Some(2 << 32)
        );
        assert_eq!(
            ErasedTypeId::of(Type::String).map(ErasedTypeId::as_u64),
            Some(3 << 32)
        );
    }

    #[test]
    fn a_raw_word_round_trips() {
        let id = ErasedTypeId::of(Type::String).expect("String erases");
        assert_eq!(ErasedTypeId::from_u64(id.as_u64()), id);
    }
}
