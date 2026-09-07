//! The integer widths the language spells, as every engine must honor them.
//!
//! An integer travels as one 64-bit word on every engine, extended from its
//! spelling's width. The spelling still decides what the word *means*: the
//! range arithmetic may not leave, the count a shift is measured against,
//! whether the top bit is a sign, and how a conversion to another spelling
//! is checked. Those rules are here, shared by the compiler that emits the
//! checks and the runtimes that perform them.

/// A written integer width and signedness. `Plain` is bare `Int`: 64 bits,
/// signed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntWidth {
    /// Bare `Int`, 64 bits, signed.
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

impl IntWidth {
    /// The width in bits: the range a value may hold and the count a shift is
    /// measured against.
    #[must_use]
    pub fn bits(self) -> u32 {
        match self {
            IntWidth::I8 | IntWidth::U8 => 8,
            IntWidth::I16 | IntWidth::U16 => 16,
            IntWidth::I32 | IntWidth::U32 => 32,
            IntWidth::Plain | IntWidth::U64 => 64,
        }
    }

    /// Whether the top bit is a sign.
    #[must_use]
    pub fn is_signed(self) -> bool {
        matches!(
            self,
            IntWidth::Plain | IntWidth::I8 | IntWidth::I16 | IntWidth::I32
        )
    }

    /// The spelling as written in source.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            IntWidth::Plain => "Int",
            IntWidth::I8 => "I8",
            IntWidth::I16 => "I16",
            IntWidth::I32 => "I32",
            IntWidth::U8 => "U8",
            IntWidth::U16 => "U16",
            IntWidth::U32 => "U32",
            IntWidth::U64 => "U64",
        }
    }

    /// The smallest and largest value the width represents.
    #[must_use]
    pub fn range(self) -> (i128, i128) {
        let bits = self.bits();
        if self.is_signed() {
            (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1)
        } else {
            (0, (1i128 << bits) - 1)
        }
    }

    /// Whether `value` lies in the range.
    #[must_use]
    pub fn holds(self, value: i128) -> bool {
        let (low, high) = self.range();
        (low..=high).contains(&value)
    }

    /// The value a runtime word denotes under this width.
    ///
    /// Only `U64` can hold a pattern whose top bit is not a sign, so only
    /// `U64` reads its word unsigned; every narrower width is stored extended
    /// to 64 bits and reads as a signed word.
    #[must_use]
    pub fn value_of(self, word: i64) -> i128 {
        match self {
            IntWidth::U64 => i128::from(word as u64),
            _ => i128::from(word),
        }
    }

    /// The runtime word that denotes `value`, which must lie in the range.
    #[must_use]
    pub fn word_of(self, value: i128) -> i64 {
        match self {
            IntWidth::U64 => value as u64 as i64,
            _ => value as i64,
        }
    }

    /// `word` reduced to this width the way a shift discards bits: the low
    /// `bits` kept, then sign- or zero-extended.
    #[must_use]
    pub fn wrap(self, word: i64) -> i64 {
        let bits = self.bits();
        if bits == 64 {
            return word;
        }
        let mask = (1u64 << bits) - 1;
        let low = (word as u64) & mask;
        if self.is_signed() && (low >> (bits - 1)) & 1 == 1 {
            (low | !mask) as i64
        } else {
            low as i64
        }
    }

    /// Whether every value of this width is a value of `to`, so a conversion
    /// needs no runtime check.
    #[must_use]
    pub fn widens_into(self, to: IntWidth) -> bool {
        let (low, high) = self.range();
        to.holds(low) && to.holds(high)
    }

    /// A one-byte code an instruction stream carries the width as.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            IntWidth::Plain => 0,
            IntWidth::I8 => 1,
            IntWidth::I16 => 2,
            IntWidth::I32 => 3,
            IntWidth::U8 => 4,
            IntWidth::U16 => 5,
            IntWidth::U32 => 6,
            IntWidth::U64 => 7,
        }
    }

    /// The width a [`code`](Self::code) names.
    #[must_use]
    pub fn from_code(code: u8) -> Option<IntWidth> {
        Some(match code {
            0 => IntWidth::Plain,
            1 => IntWidth::I8,
            2 => IntWidth::I16,
            3 => IntWidth::I32,
            4 => IntWidth::U8,
            5 => IntWidth::U16,
            6 => IntWidth::U32,
            7 => IntWidth::U64,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::IntWidth;

    #[test]
    fn ranges_follow_the_written_width() {
        assert_eq!(IntWidth::U8.range(), (0, 255));
        assert_eq!(IntWidth::I8.range(), (-128, 127));
        assert_eq!(IntWidth::U64.range(), (0, u64::MAX.into()));
        assert_eq!(IntWidth::Plain.range(), (i64::MIN.into(), i64::MAX.into()));
    }

    #[test]
    fn a_u64_word_reads_unsigned_and_writes_back() {
        let word = -1i64;
        assert_eq!(IntWidth::U64.value_of(word), u64::MAX.into());
        assert_eq!(IntWidth::U64.word_of(u64::MAX.into()), -1);
        assert_eq!(IntWidth::Plain.value_of(word), -1);
    }

    #[test]
    fn wrapping_keeps_the_low_bits_and_extends() {
        assert_eq!(IntWidth::U8.wrap(0x1ff), 0xff);
        assert_eq!(IntWidth::I8.wrap(0x80), -128);
        assert_eq!(IntWidth::I8.wrap(0x7f), 127);
        assert_eq!(IntWidth::U16.wrap(-1), 0xffff);
        assert_eq!(IntWidth::Plain.wrap(-1), -1);
    }

    #[test]
    fn widening_is_by_range() {
        assert!(IntWidth::U8.widens_into(IntWidth::I16));
        assert!(IntWidth::U32.widens_into(IntWidth::Plain));
        assert!(!IntWidth::U64.widens_into(IntWidth::Plain));
        assert!(!IntWidth::I8.widens_into(IntWidth::U8));
        assert!(!IntWidth::Plain.widens_into(IntWidth::U64));
    }

    #[test]
    fn codes_round_trip() {
        for code in 0..8 {
            assert_eq!(IntWidth::from_code(code).map(IntWidth::code), Some(code));
        }
        assert_eq!(IntWidth::from_code(8), None);
    }
}
