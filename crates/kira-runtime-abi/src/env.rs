//! The environment capability: what the process was started with.
//!
//! An environment variable is not a filesystem read and not a compiler service,
//! but it reaches a Kira program the same way both of those do — as an
//! intrinsic the VM routes through its host and native code routes through a
//! `kira_rt_*` symbol. It gets that treatment for one reason: the alternative
//! is every caller declaring `getenv` as an `@FFI.Extern`, and a library that
//! does binds a C symbol through whichever native library it happened to name.
//! The UI Foundation did exactly that — it read `KIRA_UI_*` variables through
//! `library: kira_metal`, so a compositor that wanted to know whether a debug
//! flag was set could not link on a machine with no Metal.
//!
//! # Nothing here fails
//!
//! An unset variable is not an error, it is an answer: [`EnvOp::Text`] gives
//! back the empty string and [`EnvOp::IsSet`] gives back `false`. Both are
//! offered, because "" and unset are different states and a program that can
//! only ask for the text cannot tell them apart — `KIRA_UI_TRACE=` is a
//! deliberate empty setting, not an absent one.
//!
//! # Read, never written
//!
//! There is no set operation, and adding one would need a reason this does not
//! have. The environment is process-wide mutable state shared with every
//! library in the address space, and `setenv` is not thread-safe against a
//! concurrent `getenv` on any platform Kira targets.

/// Which environment operation one `EnvOp` instruction performs.
///
/// The discriminants are a wire contract: they travel in the operand byte of
/// the `EnvOp` bytecode instruction, so they are **append-only** — a new
/// operation takes the next free number and no existing one ever moves.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum EnvOp {
    /// The variable's value, empty when it is unset.
    Text = 0,
    /// Whether the variable is set at all, however empty its value.
    IsSet = 1,
}

impl EnvOp {
    /// Every operation, in wire order.
    ///
    /// The one place the set is written down: decoding indexes this rather than
    /// repeating a match, so a new operation cannot be added to the enum and
    /// forgotten by the decoder.
    pub const ALL: [EnvOp; 2] = [EnvOp::Text, EnvOp::IsSet];

    /// The wire byte this operation travels as.
    pub const fn as_byte(self) -> u8 {
        self as u8
    }

    /// Reads a wire byte, or `None` when it names no operation.
    pub fn from_byte(byte: u8) -> Option<Self> {
        Self::ALL.get(usize::from(byte)).copied()
    }

    /// How many operands this operation pops, in source order.
    pub const fn arity(self) -> usize {
        match self {
            EnvOp::Text | EnvOp::IsSet => 1,
        }
    }

    /// The Kira intrinsic name that compiles to this operation.
    pub const fn intrinsic_name(self) -> &'static str {
        match self {
            EnvOp::Text => "envText",
            EnvOp::IsSet => "envIsSet",
        }
    }

    /// The `kira_rt_*` symbol native code calls to perform this operation.
    ///
    /// Derived from the operation rather than written twice, so the backend's
    /// declaration and the runtime's definition cannot drift apart.
    pub const fn runtime_symbol(self) -> &'static str {
        match self {
            EnvOp::Text => "kira_rt_env_text",
            EnvOp::IsSet => "kira_rt_env_is_set",
        }
    }

    /// Resolves a Kira intrinsic name to its operation, or `None`.
    pub fn from_intrinsic_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|op| op.intrinsic_name() == name)
    }
}

/// Reads `name` from the process environment.
///
/// One definition for every engine: the VM calls it from its interpreter and
/// native code reaches it through `kira_rt_env_text`, so a program cannot get
/// one answer on one backend and another on the next.
///
/// A name holding an interior NUL, or a value that is not UTF-8, reads as
/// unset — neither is something a Kira `String` can carry, and inventing a
/// lossy answer would be worse than saying there is nothing there.
#[must_use]
pub fn text(name: &str) -> String {
    std::env::var(name).unwrap_or_default()
}

/// Whether `name` is set, however empty its value.
#[must_use]
pub fn is_set(name: &str) -> bool {
    std::env::var_os(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_bytes_are_pinned() {
        // These travel in an operand byte that artifacts already hold.
        assert_eq!(EnvOp::Text.as_byte(), 0);
        assert_eq!(EnvOp::IsSet.as_byte(), 1);
        assert_eq!(EnvOp::from_byte(0), Some(EnvOp::Text));
        assert_eq!(EnvOp::from_byte(1), Some(EnvOp::IsSet));
        assert_eq!(EnvOp::from_byte(2), None);
    }

    #[test]
    fn every_operation_round_trips_through_its_names() {
        for op in EnvOp::ALL {
            assert_eq!(EnvOp::from_intrinsic_name(op.intrinsic_name()), Some(op));
            assert_eq!(EnvOp::from_byte(op.as_byte()), Some(op));
            assert!(op.runtime_symbol().starts_with("kira_rt_env_"));
        }
    }

    #[test]
    fn an_unset_variable_is_an_answer_rather_than_a_failure() {
        let absent = "KIRA_TEST_ENV_DEFINITELY_UNSET";
        assert_eq!(text(absent), "");
        assert!(!is_set(absent));
    }

    #[test]
    fn a_variable_set_to_nothing_is_still_set() {
        // The distinction the two operations exist to draw: `KIRA_UI_TRACE=` is
        // a deliberate empty setting, not an absent one.
        let name = "KIRA_TEST_ENV_SET_TO_EMPTY";
        // SAFETY: this process's own variable, named for this test alone, and
        // read back on this thread before anything else can observe it.
        unsafe { std::env::set_var(name, "") };
        assert_eq!(text(name), "");
        assert!(is_set(name));
        // SAFETY: as above — removing what this test just set.
        unsafe { std::env::remove_var(name) };
    }
}
