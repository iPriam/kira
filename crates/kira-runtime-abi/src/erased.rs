//! What a value of the top type erased, at the native ABI.
//!
//! Native code has nowhere to keep a value's type once the static type stops
//! saying it, so an `Any` on that side is a box: a tag naming the kind that was
//! erased, what the payload word owns (an [`EnumPayloadKind`]), and the word
//! itself. The box's shape is the enum box's, deliberately — a copy is a share
//! bump and a drop is `kira_rt_enum_free` either way, so erasure reuses two code
//! paths that already exist instead of adding a third.
//!
//! [`EnumPayloadKind`]: crate::EnumPayloadKind
//!
//! The VM writes none of this. Its `Value` is already a tagged union, so an
//! erased value there *is* the value, and the tag it needs is the one it already
//! carries. This is the native half of one design, not a second one.
//!
//! # Why the tag is written when nothing reads it
//!
//! The language has no `is`, `as`, or downcast surface, so no Kira program can
//! ask what an `Any` erased. The box carries a tag field regardless — it is the
//! enum box's shape — and writing the real kind into it costs one constant.
//! Writing zero instead would make the representation unrecoverable by
//! construction, which a later recovery form could not fix without changing
//! every box already emitted.
//!
//! Append-only, like every wire tag here: a new kind goes on the end.

/// Which kind of value an `Any` box erased.
///
/// A transparent newtype rather than a Rust `enum` for the reason every wire tag
/// here is one: an out-of-range discriminant in a Rust `enum` is undefined
/// behavior, while an unrecognized value here is simply one nothing interprets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct ErasedKind(pub i64);

impl ErasedKind {
    /// An integer of any spelling. Every width shares one 64-bit
    /// representation, so the spelling is not part of the tag.
    pub const INT: Self = Self(0);
    /// A float of any spelling, stored as its IEEE-754 bits.
    pub const FLOAT: Self = Self(1);
    /// A boolean, stored as a zero-extended word.
    pub const BOOL: Self = Self(2);
    /// An owned string handle.
    pub const STRING: Self = Self(3);
    /// An owned struct value, held in the runtime's erased aggregate payload.
    pub const STRUCT: Self = Self(4);
    /// An owned array handle, held in the erased aggregate payload for the same
    /// reason a struct is: the clone and free it needs are type-specific.
    pub const ARRAY: Self = Self(5);
    /// An owned enum handle.
    pub const ENUM: Self = Self(6);
    /// An opaque foreign pointer word, which owns nothing.
    pub const RAW_PTR: Self = Self(7);

    /// The raw word this kind is written as.
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::ErasedKind;

    /// Spelled out literally, so a renumbering is a failing test rather than a
    /// silent change to what every `Any` box already emitted claims to hold.
    #[test]
    fn erased_kind_wire_bytes_are_pinned() {
        assert_eq!(ErasedKind::INT.as_i64(), 0);
        assert_eq!(ErasedKind::FLOAT.as_i64(), 1);
        assert_eq!(ErasedKind::BOOL.as_i64(), 2);
        assert_eq!(ErasedKind::STRING.as_i64(), 3);
        assert_eq!(ErasedKind::STRUCT.as_i64(), 4);
        assert_eq!(ErasedKind::ARRAY.as_i64(), 5);
        assert_eq!(ErasedKind::ENUM.as_i64(), 6);
        assert_eq!(ErasedKind::RAW_PTR.as_i64(), 7);
    }

    /// The tag travels in the enum box's `tag` field, which is one word.
    #[test]
    fn the_kind_is_word_sized() {
        assert_eq!(size_of::<ErasedKind>(), size_of::<i64>());
    }

    /// Every kind is distinct: two that collided would make one unrecoverable.
    #[test]
    fn the_kinds_are_distinct() {
        let all = [
            ErasedKind::INT,
            ErasedKind::FLOAT,
            ErasedKind::BOOL,
            ErasedKind::STRING,
            ErasedKind::STRUCT,
            ErasedKind::ARRAY,
            ErasedKind::ENUM,
            ErasedKind::RAW_PTR,
        ];
        for (index, kind) in all.iter().enumerate() {
            assert!(
                !all[..index].contains(kind),
                "{kind:?} repeats an earlier kind"
            );
        }
    }
}
