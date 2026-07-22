//! How a parameter takes its argument, and therefore who frees it.
//!
//! Kira's ownership model is Rust's. That is not an analogy — it is the same
//! model, which is why the compiler is written in Rust: the host language's
//! checker enforces the guest language's rule, and every value crossing this
//! crate's boundaries can be represented by a Rust type with the same meaning.
//!
//! | Kira | Rust | Who frees |
//! |---|---|---|
//! | `function f(x: Any)` | `fn f(x: T)` | the callee — the argument is moved in |
//! | `function f(x: borrow Any)` | `fn f(x: &T)` | the caller — the callee only reads |
//! | `function f(x: borrow mut Any)` | `fn f(x: &mut T)` | the caller — the callee may write |
//! | `f(move x)` | `f(x)` | the callee; `x` is dead afterwards |
//! | `Int`, `Float`, `Bool` | `Copy` types | nobody — trivial values are copied |
//!
//! Two consequences worth stating, because they are what make this enum small:
//!
//! - **`owned` and `move` are the same mode.** A by-value parameter *is* a move;
//!   `move` is call-site syntax that names which binding is being given up, not
//!   a different way to pass. So there is no `Move` variant here.
//! - **`copy` is a property of the type, not the parameter.** A trivial value is
//!   copied at every mode, and owns nothing to free either way, so it needs no
//!   mode of its own — exactly as Rust's `Copy` is a trait, not a parameter
//!   annotation.
//!
//! # Why the boundary needs this
//!
//! Inside one engine, the borrow checker settles ownership at compile time and
//! nothing needs to be recorded. A call that crosses between the VM and native
//! code cannot: the two sides are separately compiled, and machine code has no
//! reflection to ask. The mode therefore travels in the hybrid manifest, and the
//! runtime reads it to decide whether a value it hands over is a transfer (the
//! other side frees) or a loan (it keeps the value and frees it itself).

/// How a parameter takes its argument.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Ownership {
    /// By value: the argument is moved in and the callee frees it (`Any`).
    ///
    /// The default, because Kira owns by default.
    #[default]
    Owned,
    /// A read-only borrow: the caller keeps the value and frees it (`&T`).
    Borrow,
    /// A mutable borrow: the caller keeps the value and frees it (`&mut T`).
    BorrowMut,
}

impl Ownership {
    /// Whether a callee receiving this mode becomes responsible for freeing.
    ///
    /// The single question the hybrid boundary actually asks.
    pub fn transfers_ownership(self) -> bool {
        matches!(self, Ownership::Owned)
    }

    /// Whether the callee may write through this parameter.
    pub fn is_mutable(self) -> bool {
        matches!(self, Ownership::BorrowMut)
    }

    /// The Kira syntax that selects this mode, as it precedes a type name.
    pub fn syntax(self) -> &'static str {
        match self {
            Ownership::Owned => "",
            Ownership::Borrow => "borrow ",
            Ownership::BorrowMut => "borrow mut ",
        }
    }

    /// The wire encoding of this mode.
    ///
    /// Hybrid manifests carry this byte, so the values are append-only: never
    /// renumber one.
    pub fn as_byte(self) -> u8 {
        match self {
            Ownership::Owned => 0,
            Ownership::Borrow => 1,
            Ownership::BorrowMut => 2,
        }
    }

    /// Decodes a wire byte, or `None` when it names no mode.
    ///
    /// A manifest is a deserializable public artifact, so an unknown byte is a
    /// rejection rather than a panic.
    pub fn from_byte(byte: u8) -> Option<Ownership> {
        Some(match byte {
            0 => Ownership::Owned,
            1 => Ownership::Borrow,
            2 => Ownership::BorrowMut,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_owned_parameter_transfers_the_obligation_to_free() {
        assert!(Ownership::Owned.transfers_ownership());
        assert!(!Ownership::Borrow.transfers_ownership());
        assert!(!Ownership::BorrowMut.transfers_ownership());
    }

    #[test]
    fn only_a_mutable_borrow_may_be_written_through() {
        assert!(Ownership::BorrowMut.is_mutable());
        assert!(!Ownership::Borrow.is_mutable());
        assert!(!Ownership::Owned.is_mutable());
    }

    #[test]
    fn kira_owns_by_default() {
        assert_eq!(Ownership::default(), Ownership::Owned);
        assert_eq!(Ownership::Owned.syntax(), "");
    }

    #[test]
    fn wire_bytes_round_trip_and_reject_unknown() {
        for ownership in [Ownership::Owned, Ownership::Borrow, Ownership::BorrowMut] {
            assert_eq!(Ownership::from_byte(ownership.as_byte()), Some(ownership));
        }
        assert_eq!(Ownership::from_byte(3), None);
    }
}
