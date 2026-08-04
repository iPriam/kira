//! The string operations that travel as one opcode with an operand byte.
//!
//! Kira's first string primitives — `count`, `charAt`, `indexOf`,
//! `substring` — each took an opcode of their own. That is affordable four
//! times and wasteful thereafter: the opcode space is one byte, and a language
//! that expects to keep growing its string surface should not spend a scarce
//! number every time it does.
//!
//! So these share a single `StringOp` instruction and are told apart by the
//! byte that follows it, exactly as [`FileSystemOp`](crate::FileSystemOp),
//! `TaskPrim` and `CompilerOp` already are. A new string operation costs a
//! number in *this* enum and nothing in the opcode table.
//!
//! # Why these operations and not others
//!
//! Every one of them answers a question about text that a program cannot
//! answer for itself without writing a loop. `contains` is expressible as
//! `indexOf(n) >= 0` and `startsWith` as a `substring` compare, but a language
//! whose users must derive those has a string type in name only — and each
//! derivation scans the text twice where the primitive scans it once.

/// Which string operation one `StringOp` instruction performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `StringOp` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum StringOp {
    /// Whether the text holds `needle` anywhere.
    Contains = 0,
    /// Whether the text begins with `prefix`.
    StartsWith = 1,
    /// Whether the text ends with `suffix`.
    EndsWith = 2,
    /// The text split on every occurrence of a separator.
    Split = 3,
    /// The text with every occurrence of one run replaced by another.
    Replace = 4,
    /// The text without leading or trailing whitespace.
    Trim = 5,
    /// The text with every character lowercased.
    Lowercase = 6,
    /// The text with every character uppercased.
    Uppercase = 7,
}

impl StringOp {
    /// Every operation, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new operation cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [StringOp; 8] = [
        StringOp::Contains,
        StringOp::StartsWith,
        StringOp::EndsWith,
        StringOp::Split,
        StringOp::Replace,
        StringOp::Trim,
        StringOp::Lowercase,
        StringOp::Uppercase,
    ];

    /// The wire byte this operation travels as.
    #[must_use]
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    ///
    /// A decoder never guesses: an unknown byte is rejected by its caller
    /// rather than folded into a neighbouring operation.
    #[must_use]
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// The method name a Kira program writes to reach this operation.
    #[must_use]
    pub const fn method_name(self) -> &'static str {
        match self {
            StringOp::Contains => "contains",
            StringOp::StartsWith => "startsWith",
            StringOp::EndsWith => "endsWith",
            StringOp::Split => "split",
            StringOp::Replace => "replace",
            StringOp::Trim => "trim",
            StringOp::Lowercase => "lowercase",
            StringOp::Uppercase => "uppercase",
        }
    }

    /// The operation a method name reaches, or `None` when it names none.
    #[must_use]
    pub fn from_method_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.method_name() == name)
    }

    /// How many arguments the method takes, the receiver excluded.
    #[must_use]
    pub const fn argument_count(self) -> usize {
        match self {
            StringOp::Trim | StringOp::Lowercase | StringOp::Uppercase => 0,
            StringOp::Contains | StringOp::StartsWith | StringOp::EndsWith | StringOp::Split => 1,
            StringOp::Replace => 2,
        }
    }

    /// Whether the operation answers with a `Bool`.
    #[must_use]
    pub const fn answers_bool(self) -> bool {
        matches!(
            self,
            StringOp::Contains | StringOp::StartsWith | StringOp::EndsWith
        )
    }

    /// Whether the operation answers with a `[String]`.
    #[must_use]
    pub const fn answers_string_array(self) -> bool {
        matches!(self, StringOp::Split)
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    #[must_use]
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            StringOp::Contains => "kira_rt_string_contains",
            StringOp::StartsWith => "kira_rt_string_starts_with",
            StringOp::EndsWith => "kira_rt_string_ends_with",
            StringOp::Split => "kira_rt_string_split",
            StringOp::Replace => "kira_rt_string_replace",
            StringOp::Trim => "kira_rt_string_trim",
            StringOp::Lowercase => "kira_rt_string_lowercase",
            StringOp::Uppercase => "kira_rt_string_uppercase",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_bytes_are_pinned() {
        // Spelled out literally: this is what turns a reorder into a failing
        // test rather than a silent redirection of every artifact already
        // written.
        assert_eq!(StringOp::Contains.as_byte(), 0);
        assert_eq!(StringOp::StartsWith.as_byte(), 1);
        assert_eq!(StringOp::EndsWith.as_byte(), 2);
        assert_eq!(StringOp::Split.as_byte(), 3);
        assert_eq!(StringOp::Replace.as_byte(), 4);
        assert_eq!(StringOp::Trim.as_byte(), 5);
        assert_eq!(StringOp::Lowercase.as_byte(), 6);
        assert_eq!(StringOp::Uppercase.as_byte(), 7);
    }

    #[test]
    fn every_operation_round_trips_its_byte() {
        for op in StringOp::ALL {
            assert_eq!(StringOp::from_byte(op.as_byte()), Some(op));
        }
    }

    #[test]
    fn an_unknown_byte_names_no_operation() {
        assert_eq!(StringOp::from_byte(StringOp::ALL.len() as u8), None);
        assert_eq!(StringOp::from_byte(u8::MAX), None);
    }

    #[test]
    fn every_operation_round_trips_its_method_name() {
        for op in StringOp::ALL {
            assert_eq!(StringOp::from_method_name(op.method_name()), Some(op));
        }
        assert_eq!(StringOp::from_method_name("nonsense"), None);
    }

    #[test]
    fn each_operation_names_a_distinct_runtime_symbol() {
        let mut seen: Vec<&str> = StringOp::ALL.iter().map(|op| op.runtime_symbol()).collect();
        seen.sort_unstable();
        let count = seen.len();
        seen.dedup();
        assert_eq!(seen.len(), count, "two operations share a symbol");
    }
}
