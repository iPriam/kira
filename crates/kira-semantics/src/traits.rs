//! Traits: what a type can promise to present, and what promising costs.
//!
//! A trait is a named set of members. A member with no body is a
//! **requirement** every conforming type must present; a member with a body is
//! a **default** a conforming type inherits unless it writes its own. A trait
//! with no members is a **marker**: it classifies without obliging.
//!
//! # Nothing below semantics learns traits exist
//!
//! Conformance is resolved here and dispatch is static. A default a type
//! inherits is registered as one more [`Callable`] whose receiver is *that*
//! type — the same trick classes use for an inherited method — so `mesh.hash()`
//! is an ordinary direct call to `Mesh.hash` by the time the HIR exists. There
//! is no vtable, no trait object, and no runtime representation of a trait: the
//! IR, both compilers, and the hybrid manifest see functions.
//!
//! That is also why a trait names no *type*. `let x: Hashable` would need a
//! value that carries its own dispatch, which is a different feature; it is
//! refused by name rather than half-supported.
//!
//! # Compiler-known traits
//!
//! [`COPYABLE`], [`DROP`], [`SEND`], and [`SYNC`] exist without a declaration,
//! because each states something only the compiler can settle. `Copyable`,
//! `Send`, and `Sync` are *derived* from a type's own members and a written
//! claim is an assertion checked against them; `Drop` attaches a body the
//! engines run where they already release the value. None may be declared in
//! source. See [`markers`] for what `Send` and `Sync` mean and which leaves
//! settle them.
//!
//! [`Callable`]: crate::analyze::Callable

mod check;
mod conformance;
pub(crate) mod drop;
pub(crate) mod existential;
pub(crate) mod markers;

use std::collections::{BTreeMap, HashSet};

use kira_semantics_model::Type;
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{Function, TypeParamDecl, TypeRefId};

use crate::analyze::Analyzer;

/// The compiler-known trait asserting that a type copies rather than moves.
pub(crate) const COPYABLE: &str = "Copyable";

/// The compiler-known trait attaching a user body to a type's release.
pub(crate) const DROP: &str = "Drop";

/// The compiler-known trait asserting that a value may be moved to another
/// thread.
pub(crate) const SEND: &str = "Send";

/// The compiler-known trait asserting that a value may be borrowed from more
/// than one thread at once.
pub(crate) const SYNC: &str = "Sync";

/// The compiler-known trait asserting that a type has structural equality, so
/// `==` compares it and `<T: Equatable>` accepts it.
///
/// Like `Copyable`, it is *derived* from a type's shape rather than declared: a
/// type conforms when its every leaf is comparable ([`Analyzer::is_equatable`]).
/// There is nothing to write — `==` and the bound both read the same structural
/// fact — which is why it has no members and cannot be declared or `extend`ed.
///
/// [`Analyzer::is_equatable`]: crate::analyze::Analyzer::is_equatable
pub(crate) const EQUATABLE: &str = "Equatable";

/// The compiler-known trait asserting that a type has a total structural order,
/// so `<` / `<=` / `>` / `>=` compare it and `<T: Ordered>` accepts it.
///
/// The ordering sibling of [`EQUATABLE`], and stricter: it is *derived* from a
/// type's shape rather than declared, and conforms when its every leaf carries a
/// total order ([`Analyzer::ordered_refusal`]) — a number, a boolean, a string,
/// and the aggregates of those — refusing a leaf, such as a pointer word or an
/// opaque handle, whose only order would be its address. There is nothing to
/// write; the orderings and the bound read the same structural fact.
///
/// [`Analyzer::ordered_refusal`]: crate::analyze::Analyzer::ordered_refusal
pub(crate) const ORDERED: &str = "Ordered";

/// The compiler-known trait asserting that a type folds into one number
/// consistently with equality, so `hash(v)` accepts it and `<T: Hashable>` does.
///
/// Derived from a type's shape like [`EQUATABLE`], and gated on the same
/// promise equality keeps: it admits only leaves whose bits are their value, so
/// two values that are `==` hash the same. A float is refused
/// ([`Analyzer::hashable_refusal`]), because equal floats may differ in bits.
///
/// [`Analyzer::hashable_refusal`]: crate::analyze::Analyzer::hashable_refusal
pub(crate) const HASHABLE: &str = "Hashable";

/// Whether `name` is a trait the compiler knows without a declaration.
pub(crate) fn is_builtin_trait(name: &str) -> bool {
    matches!(
        name,
        COPYABLE | DROP | SEND | SYNC | EQUATABLE | ORDERED | HASHABLE
    )
}

/// Whether `name` is a compiler-known trait whose truth is *derived* from a
/// type's members rather than declared.
///
/// `Drop` is the one that is not: it attaches a body, so it is true exactly
/// where someone wrote one. The rest are facts about a shape, which is why a
/// supertrait requiring one is discharged by the fact rather than by a second
/// spelling of it.
pub(crate) fn is_derived_trait(name: &str) -> bool {
    matches!(name, COPYABLE | SEND | SYNC | EQUATABLE | ORDERED | HASHABLE)
}

/// One declared trait's members and where it was written.
#[derive(Debug, Clone)]
pub(crate) struct TraitInfo<'a> {
    /// The file the trait was declared in.
    ///
    /// Its package is one of the two that may declare a conformance to this
    /// trait, and it is the scope the member signatures resolve against.
    pub(crate) source: SourceId,
    /// The parameters on the trait declaration. Empty for an ordinary trait;
    /// a non-empty list marks this row as a template until a concrete trait
    /// instance is minted under its mangled name.
    pub(crate) type_params: Vec<TypeParamDecl>,
    /// Substitutions active while resolving this concrete trait instance's
    /// members. The template row carries an empty frame.
    pub(crate) type_bindings: crate::generics::TypeBindings,
    /// The traits this one *requires*, written `trait Ord: Eq { … }`.
    ///
    /// A supertrait is an obligation rather than an inheritance: a type
    /// claiming this trait must claim each of these too, and it takes their
    /// members from *those* conformances rather than from this one.
    pub(crate) supertraits: Vec<SupertraitRef>,
    /// The members, in declaration order.
    pub(crate) members: Vec<TraitMemberInfo<'a>>,
}

/// One trait named in another trait's supertrait clause.
#[derive(Debug, Clone)]
pub(crate) struct SupertraitRef {
    /// The required trait's name, as written.
    pub(crate) name: String,
    /// Type arguments written on the supertrait, if any.
    pub(crate) args: Vec<TypeRefId>,
    /// Span of the name at the clause, for the diagnostics that point at it.
    pub(crate) span: Span,
}

/// One member of a declared trait.
#[derive(Debug, Clone)]
pub(crate) struct TraitMemberInfo<'a> {
    /// The member's name, as written.
    pub(crate) name: String,
    /// Whether the declaration wrote no body, making the member a requirement.
    pub(crate) required: bool,
    /// The declaration as written: the signature, and a default's body.
    pub(crate) function: &'a Function,
}

/// A contract a type can be obliged to keep.
///
/// Two kinds, one table. A trait states its requirements as members with no
/// body; a construct family states them as `@Required` members. Both are "here
/// is a set of members every conforming type must present", and both are
/// checked from [`Conformance`] rows rather than from where the declaration
/// happened to be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Contract {
    /// A declared trait, by name.
    Trait(String),
    /// A construct family's own `@Required` surface, by family name.
    ///
    /// Every declaration backed by the family keeps this contract, whether or
    /// not the family names any trait.
    Family(String),
}

impl Contract {
    /// The trait this contract names, or `None` for a family's own surface.
    pub(crate) fn trait_name(&self) -> Option<&str> {
        match self {
            Contract::Trait(name) => Some(name),
            Contract::Family(_) => None,
        }
    }
}

/// One conformance a program declared: a type keeping a contract's promise.
#[derive(Debug, Clone)]
pub(crate) struct Conformance {
    /// The contract kept.
    pub(crate) contract: Contract,
    /// The conforming type.
    pub(crate) ty: Type,
    /// The file the conformance was declared in, whose package coherence is
    /// measured against.
    pub(crate) source: SourceId,
    /// Span of the trait name at the conformance site.
    pub(crate) span: Span,
    /// The construct family that claimed the trait, when this conformance is
    /// one a backed declaration inherits rather than one it wrote.
    ///
    /// A family claiming a trait obliges every declaration backed by it, so one
    /// written claim becomes one conformance per declaration. The family's name
    /// travels with each of them: a requirement the family itself answers is
    /// kept, and a refusal names both the declaration and where the claim was
    /// written.
    pub(crate) via_family: Option<String>,
    /// The member names the conforming type presents itself, so a default is
    /// inherited only where the type wrote none.
    ///
    /// Names rather than signatures: a type's method of a trait member's name
    /// *is* its answer for that member, and whether the shapes agree is the
    /// conformance check's question rather than the inheritance rule's.
    pub(crate) provided: HashSet<String>,
}

/// Every trait a program declares, keyed by name.
pub(crate) type TraitTable<'a> = BTreeMap<String, TraitInfo<'a>>;

impl<'a> Analyzer<'a> {
    /// The key under which `name`, written in the current file, finds a
    /// declared trait: the file's own package first, then the program's own
    /// declarations, then the packages the file imports. A trait is filed
    /// under its declaring package (`Pkg::Name`), so two packages may each
    /// declare a `Named`.
    pub(crate) fn visible_trait_key(&self, name: &str) -> Option<String> {
        let home = self.template_key(self.source, name);
        if self.traits.contains_key(&home) {
            return Some(home);
        }
        if self.traits.contains_key(name) {
            return Some(name.to_owned());
        }
        self.imports
            .imported_packages(self.source)
            .into_iter()
            .map(|package| format!("{package}::{name}"))
            .find(|key| {
                self.traits
                    .get(key)
                    .is_some_and(|info| self.sees_public(info.source, name))
            })
    }
}
