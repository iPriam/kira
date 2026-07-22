//! What an enum box's type-erased payload word is, at the native ABI.
//!
//! A Kira enum crosses the native boundary as a tag plus one word. That word is
//! sometimes inert bits (a scalar payload), sometimes an owned string handle,
//! and sometimes an owned nested-enum handle — and the runtime's clone and free
//! have to know which, because the box does not carry the payload's type.
//!
//! This tag is what they read. It lives here rather than in either crate that
//! uses it because *both* sides are compiled separately and must agree: the
//! LLVM backend writes the value into every `kira_rt_enum_new` call it emits,
//! and the runtime archive interprets it in `kira_rt_enum_clone` and
//! `kira_rt_enum_free`. A disagreement is silent — the symbols still resolve and
//! the box simply forgets to reclaim its payload — so one definition serves
//! both.
//!
//! Append-only, like every wire tag here: a new kind goes on the end, and an
//! unrecognized one is treated as owning nothing rather than guessed at.

/// What an enum box's payload word holds.
///
/// A transparent newtype rather than a Rust `enum` because foreign code writes
/// this byte: an out-of-range discriminant in a Rust `enum` is undefined
/// behavior, while an unknown value here is simply a value the runtime declines
/// to interpret.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct EnumPayloadKind(pub i64);

impl EnumPayloadKind {
    /// Inert bits: a scalar payload, or a variant with no payload at all. Owns
    /// nothing.
    pub const INERT: Self = Self(0);
    /// An owned string handle, cloned and freed with the box.
    pub const STR: Self = Self(1);
    /// An owned nested-enum handle, cloned and freed with the box.
    ///
    /// This is what a `Result`-shaped value's `Error` variant carries, so it is
    /// the kind `attempt`/`try`/`handle` depends on.
    pub const ENUM: Self = Self(2);
    /// An owned erased aggregate payload with compiler-generated clone/free
    /// callbacks. Used for struct payloads, including synthesized construct-family
    /// variants.
    pub const AGGREGATE: Self = Self(3);

    /// The raw word this kind is written as.
    pub const fn as_i64(self) -> i64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::EnumPayloadKind;

    /// The wire values are spelled out literally, so a renumbering is a failing
    /// test rather than a silent misread of every enum payload ever boxed.
    #[test]
    fn payload_kind_wire_bytes_are_pinned() {
        assert_eq!(EnumPayloadKind::INERT.as_i64(), 0);
        assert_eq!(EnumPayloadKind::STR.as_i64(), 1);
        assert_eq!(EnumPayloadKind::ENUM.as_i64(), 2);
        assert_eq!(EnumPayloadKind::AGGREGATE.as_i64(), 3);
    }

    /// The tag is one word wide, which is what the `kira_rt_enum_new` parameter
    /// it travels in is.
    #[test]
    fn the_kind_is_word_sized() {
        assert_eq!(size_of::<EnumPayloadKind>(), size_of::<i64>());
    }
}
