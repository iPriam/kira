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
//! Integer spellings collapse to one descriptor, and float spellings likewise.
//! This follows the rule `==` already applies to unerased operands, where
//! numerics unify before they compare (`kira-semantics`' `equality`): `Int` is a
//! wildcard assignable to and from every sized spelling, so an erased `I32` and
//! an erased `Int` holding the same bits are the same value by every other
//! measure the language offers. Everything nominal keeps its own descriptor,
//! because the language already treats it that way — including a `distinct`,
//! whose representation a backend stores but whose identity is what the
//! language says the value is.

use super::descriptor::{DescriptorFamily, TypeDescriptorTable};
use super::table::TypeTable;
use super::Type;

/// The identity an erased value carries, so two of them can be compared.
///
/// A family in the high 32 bits and a descriptor row in the low 32. Two types
/// are the same erased type exactly when their ids are equal, which is what
/// makes this comparable as one integer on both engines and across the C seam.
/// The family is carried beside the row because the runtime has one question it
/// must answer without the table: whether a payload word is a float, which
/// compares by IEEE rules rather than by bits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct ErasedTypeId(u64);

impl ErasedTypeId {
    /// The id `ty` erases under, minting its descriptor row on first mention.
    ///
    /// `None` for a type that names no value — `Void`, the error type, and the
    /// seam-local storage a program cannot hold — which is exactly the set
    /// [`Type::assignable_to`] refuses to carry into `Any`. Reaching here with
    /// one means analysis let through something it refuses, and the caller
    /// reports that rather than inventing a number.
    pub fn of(descriptors: &mut TypeDescriptorTable, types: &TypeTable, ty: Type) -> Option<Self> {
        let family = super::descriptor::family_of(ty)?;
        let index = descriptors.intern(types, ty)?;
        Some(Self::from_parts(family, index))
    }

    /// The id `ty` already erases under, or `None` when nothing interned it.
    ///
    /// The lookup half, for a backend reading a table lowering already built:
    /// minting a row there would give a type an id no other half of the same
    /// program knows.
    pub fn known(descriptors: &TypeDescriptorTable, ty: Type) -> Option<Self> {
        let family = super::descriptor::family_of(ty)?;
        Some(Self::from_parts(family, descriptors.id_of(ty)?))
    }

    /// Whether `ty` has a runtime descriptor at all, without minting one.
    ///
    /// The frontend's question: it admits or refuses `value.type`, `is`, and
    /// `as` before any table exists, because ids are lowering's to hand out.
    pub fn describable(ty: Type) -> Option<DescriptorFamily> {
        super::descriptor::family_of(ty)
    }

    /// Builds an id from the family and the descriptor row it names.
    pub fn from_parts(family: DescriptorFamily, index: u32) -> Self {
        Self((family.as_word() << 32) | u64::from(index))
    }

    /// The descriptor row this id names.
    pub fn descriptor_index(self) -> u32 {
        self.0 as u32
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
    use crate::ty::{FloatSpelling, IntSpelling};

    /// A table and the ids it mints, for a handful of types.
    fn ids(types: &TypeTable, wanted: &[Type]) -> Vec<Option<ErasedTypeId>> {
        let mut descriptors = TypeDescriptorTable::new();
        wanted
            .iter()
            .map(|&ty| ErasedTypeId::of(&mut descriptors, types, ty))
            .collect()
    }

    #[test]
    fn every_integer_spelling_is_one_erased_type() {
        let types = TypeTable::new();
        let minted = ids(
            &types,
            &[
                Type::Int(IntSpelling::Plain),
                Type::Int(IntSpelling::I32),
                Type::Int(IntSpelling::U8),
            ],
        );
        assert_eq!(minted[0], minted[1]);
        assert_eq!(minted[0], minted[2]);
    }

    #[test]
    fn every_float_spelling_is_one_erased_type() {
        let types = TypeTable::new();
        let minted = ids(
            &types,
            &[
                Type::Float(FloatSpelling::Plain),
                Type::Float(FloatSpelling::F32),
            ],
        );
        assert_eq!(minted[0], minted[1]);
    }

    #[test]
    fn the_scalar_families_are_distinct() {
        let types = TypeTable::new();
        let minted = ids(
            &types,
            &[
                Type::Int(IntSpelling::Plain),
                Type::Float(FloatSpelling::Plain),
                Type::Bool,
                Type::String,
                Type::RawPtr,
            ],
        );
        for (index, one) in minted.iter().enumerate() {
            assert!(one.is_some());
            for other in &minted[index + 1..] {
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
        let mut descriptors = TypeDescriptorTable::new();
        let ints_id = ErasedTypeId::of(&mut descriptors, &types, ints);
        let bools_id = ErasedTypeId::of(&mut descriptors, &types, bools);
        assert_ne!(ints_id, bools_id);
        // Interning means the same element type gives the same id back.
        let again = types.array_of(Type::Int(IntSpelling::Plain));
        assert_eq!(
            ints_id,
            ErasedTypeId::of(&mut descriptors, &types, again)
        );
        assert_eq!(ints_id, ErasedTypeId::known(&descriptors, ints));
    }

    #[test]
    fn a_type_that_never_erases_has_no_id() {
        let types = TypeTable::new();
        let minted = ids(&types, &[Type::Void, Type::Any, Type::Error]);
        assert_eq!(minted, vec![None, None, None]);
    }

    /// The encoding is a wire contract on both engines; pin the family words.
    #[test]
    fn the_family_words_are_pinned() {
        let types = TypeTable::new();
        let mut descriptors = TypeDescriptorTable::new();
        let int = ErasedTypeId::of(&mut descriptors, &types, Type::Int(IntSpelling::Plain))
            .expect("`Int` erases");
        let boolean =
            ErasedTypeId::of(&mut descriptors, &types, Type::Bool).expect("`Bool` erases");
        let text =
            ErasedTypeId::of(&mut descriptors, &types, Type::String).expect("`String` erases");
        assert_eq!(int.as_u64() >> 32, 0);
        assert_eq!(boolean.as_u64() >> 32, 2);
        assert_eq!(text.as_u64() >> 32, 3);
        // The rows are interned in mention order, so the low half is the table
        // index rather than anything about the type.
        assert_eq!(int.descriptor_index(), 0);
        assert_eq!(boolean.descriptor_index(), 1);
        assert_eq!(text.descriptor_index(), 2);
    }

    #[test]
    fn a_raw_word_round_trips() {
        let types = TypeTable::new();
        let mut descriptors = TypeDescriptorTable::new();
        let id = ErasedTypeId::of(&mut descriptors, &types, Type::String).expect("String erases");
        assert_eq!(ErasedTypeId::from_u64(id.as_u64()), id);
    }
}
