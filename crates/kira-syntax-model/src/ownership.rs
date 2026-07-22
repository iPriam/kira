//! The five closed ownership modes, as written in the source.
//!
//! The set is closed: a Kira value crosses a binding or a call boundary in
//! exactly one of these five ways and there is no sixth. The mode is a *syntax*
//! fact — it is what the source wrote — so it is anchored here in the syntax
//! model and re-exported by the semantics model rather than defined twice.
//!
//! Two spellings reach the source. A **parameter** declares the mode its
//! callee wants (`borrow Mesh`, `borrow mut Mesh`, `move Mesh`, `copy Mesh`,
//! or a bare `Mesh` for [`OwnershipMode::Owned`]). An **argument** or
//! initializer states the transfer the caller intends (`move mesh`,
//! `copy mesh`). `borrow` never appears at a call site: a borrow is the
//! callee's request, not the caller's offer, so `f(v)` is how a borrow is
//! passed.

/// How a value crosses a binding or call boundary.
///
/// Closed by design — the language has these five modes and no others. In
/// particular there is no "lease", no "share", and no borrow with a lifetime:
/// a borrow lives exactly as long as the call it is passed to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwnershipMode {
    /// The default: the callee takes ownership and drops the value.
    ///
    /// A named non-trivial argument must say `move` to reach one, which is
    /// what makes the transfer visible at the call site.
    Owned,
    /// A read-only, non-consuming borrow (`borrow Any`).
    ///
    /// The caller keeps ownership and may keep using the value afterwards.
    BorrowRead,
    /// A mutable, non-consuming borrow (`borrow mut Any`).
    ///
    /// The caller keeps ownership, the callee may write through it, and the
    /// caller's binding must be mutable ([`super::ast::Local::Var`]-like).
    BorrowMut,
    /// An explicit ownership transfer (`move Any` as a parameter, `move e` as an
    /// argument).
    Move,
    /// An explicit copy (`copy Any` as a parameter, `copy e` as an argument).
    ///
    /// Only a trivially-copyable value can actually be copied today; anything
    /// else is rejected rather than deep-cloned.
    Copy,
}

impl OwnershipMode {
    /// Whether this mode borrows rather than transfers or copies.
    ///
    /// The two borrow modes share every rule that matters at a call site: the
    /// caller keeps the value, and `move` is rejected on the argument.
    pub fn is_borrow(self) -> bool {
        matches!(self, OwnershipMode::BorrowRead | OwnershipMode::BorrowMut)
    }

    /// Whether this mode consumes the argument it is given.
    ///
    /// [`OwnershipMode::Owned`] and [`OwnershipMode::Move`] are the same rule
    /// at a call site — a bare `Any` parameter consumes exactly as a `move Any`
    /// one does — which is why the checker branches on this rather than on the
    /// two variants separately.
    pub fn consumes(self) -> bool {
        matches!(self, OwnershipMode::Owned | OwnershipMode::Move)
    }

    /// The mode as it is written in a parameter list, for diagnostics.
    pub fn spelling(self) -> &'static str {
        match self {
            OwnershipMode::Owned => "owned",
            OwnershipMode::BorrowRead => "borrow",
            OwnershipMode::BorrowMut => "borrow mut",
            OwnershipMode::Move => "move",
            OwnershipMode::Copy => "copy",
        }
    }
}

/// The ownership operators an *expression* may carry: `move e` and `copy e`.
///
/// A separate, smaller enum rather than a reuse of [`OwnershipMode`] because
/// the other three modes cannot be written on an expression, and a type that
/// cannot represent them needs no arm rejecting them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwnershipOp {
    /// `move e` — transfer ownership out of `e`.
    Move,
    /// `copy e` — produce an independent copy of `e`.
    Copy,
}

impl OwnershipOp {
    /// The operator as written, for diagnostics.
    pub fn spelling(self) -> &'static str {
        match self {
            OwnershipOp::Move => "move",
            OwnershipOp::Copy => "copy",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn borrow_modes_are_the_two_that_do_not_consume() {
        for mode in [OwnershipMode::BorrowRead, OwnershipMode::BorrowMut] {
            assert!(mode.is_borrow());
            assert!(!mode.consumes());
        }
        for mode in [OwnershipMode::Owned, OwnershipMode::Move] {
            assert!(!mode.is_borrow());
            assert!(mode.consumes());
        }
        // `copy` neither borrows nor consumes: the caller keeps the value and
        // the callee gets an independent one.
        assert!(!OwnershipMode::Copy.is_borrow());
        assert!(!OwnershipMode::Copy.consumes());
    }
}
