//! Trait declarations and the conformance clause every declaration form shares.
//!
//! A trait names a set of members a type may promise to present. A member with
//! no body is a **requirement**; a member with a body is an inheritable
//! **default** a conforming type may replace. Nothing here is a runtime shape:
//! conformance is resolved statically, so a call on a conforming value lowers
//! to an ordinary direct call and no backend learns traits exist.
//!
//! Two clauses can follow a declaration's name, in this order and with these
//! meanings, everywhere both are legal:
//!
//! ```text
//! class Panel: Drawable, Drop extends Surface { … }
//! //          ^^^^^^^^^^^^^^^ conformance      ^^^^^^^ parent
//! ```
//!
//! `:` is *always* conformance and `extends` is *always* a parent. That is why
//! [`TraitRef`] is one node reused by [`StructDecl`](super::StructDecl),
//! [`ClassDecl`](super::ClassDecl), [`ConstructDecl`](super::ConstructDecl), and
//! [`ExtendDecl`](super::ExtendDecl) rather than four clause types that would
//! each have to restate the rule.

use super::{Function, TypeParamDecl, TypeRefId};
use kira_core::Symbol;
use kira_source::Span;

/// One trait named in a `: Trait, Trait` conformance list.
///
/// A generic trait is named with its type arguments (`Producer<Int>`), so the
/// reference carries them when they were written. Empty for an ordinary trait
/// name; whether the arguments fit the trait's parameters is semantics'
/// question, which is why every name and list written is recorded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraitRef {
    /// The trait's name, as written.
    pub name: Symbol,
    /// Span of the name token, for diagnostics and definition links.
    pub span: Span,
    /// The written type arguments, in order; empty when none were written.
    pub args: Vec<TypeRefId>,
}

/// A `trait Name { … }` declaration.
///
/// A trait with no members is a **marker**: it carries no obligation and exists
/// so a type can be classified. That is a real declaration rather than a
/// degenerate one, which is why the member list is simply allowed to be empty.
#[derive(Debug, Clone, PartialEq)]
pub struct TraitDecl {
    /// The trait's name.
    pub name: Symbol,
    /// Span of the name token, for diagnostics.
    pub name_span: Span,
    /// The declared type parameters, in order; empty for an ordinary trait.
    ///
    /// A generic trait names no contract by itself: each written instantiation
    /// (`Producer<Int>` in a conformance clause or a bound) is what states the
    /// concrete one, with the arguments substituted into the members.
    pub type_params: Vec<TypeParamDecl>,
    /// Trait names written after the declaration's own name.
    ///
    /// A supertrait is a *requirement*: conforming to this trait obliges a type
    /// to conform to each of these too. It is not inheritance — this trait
    /// takes none of their members into itself.
    pub supertraits: Vec<TraitRef>,
    /// The members, in declaration order.
    pub members: Vec<TraitMember>,
    /// Span covering the whole declaration.
    pub span: Span,
}

/// One member of a [`TraitDecl`].
#[derive(Debug, Clone, PartialEq)]
pub struct TraitMember {
    /// Whether the declaration wrote a body.
    ///
    /// `false` makes the member a **requirement**: every conforming type must
    /// present it, and [`function`](Self::function) then carries an empty body
    /// that is the absence of one rather than a body to inherit. `true` makes
    /// it a **default** a conforming type inherits unless it writes its own.
    pub has_body: bool,
    /// The member's signature and, for a default, its body.
    pub function: Function,
}

/// The receiver a method declares, written as a leading `self` parameter.
///
/// Omitting it is the common case and means [`OwnershipMode::BorrowRead`]: a
/// method reads its receiver and leaves it usable, which is what every method
/// in the language has always done. Writing it says the same thing explicitly,
/// or asks for the one other mode a receiver can take — `borrow mut self`, for
/// a method that writes through the value it was called on.
///
/// [`OwnershipMode::BorrowRead`]: crate::ownership::OwnershipMode::BorrowRead
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverDecl {
    /// Whether the receiver was written `borrow mut self`.
    pub mutable: bool,
    /// Span covering the written receiver, for diagnostics.
    pub span: Span,
}
