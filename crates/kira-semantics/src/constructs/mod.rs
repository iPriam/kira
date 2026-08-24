//! Executable construct declaration families and heterogeneous `Any Family` values.
//!
//! A construct-backed declaration remains a class-shaped struct. Its construction
//! inputs and stored members are fields, computed members are zero-argument
//! methods, and ordinary members are methods. A construct family additionally
//! becomes a synthesized enum whose variants carry those concrete structs:
//!
//! ```text
//! Any Widget = Text(Text) | VStack(VStack) | Button(Button) | ...
//! ```
//!
//! The enum is declared as an empty header before structs resolve and filled once
//! every backed struct id exists. That two-phase registration is what permits a
//! backed struct to hold `Any Widget` while `Any Widget` carries that backed
//! struct. Calls on the family value become synthesized tag dispatchers, so every
//! backend executes ordinary enum projection, branching, and direct calls.

use std::collections::{BTreeMap, HashSet};

use kira_semantics_model::hir::{FuncId, HirExprId};
use kira_semantics_model::{EnumId, OwnershipMode, StructId, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::{ExprId, Function, TypeRefId};

mod backed;
mod collection;
mod construction;
mod dispatch;
mod extend;
mod inferred;
mod inherit;
mod inits;
mod slots;
mod updates;
mod value_members;

pub(crate) use dispatch::ConstructCallContent;

/// Everything analysis remembers about one construct-backed declaration beyond
/// its struct shape.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConstructInfo {
    /// Computed members read as properties rather than fields.
    pub(crate) computed: HashSet<String>,
    /// The family this declaration is backed by, as written.
    pub(crate) family: String,
    /// Every member name the declaration presents *itself*: its construction
    /// parameters, its `let` members, and its methods.
    ///
    /// This is what discharges a family requirement, and it is recorded here
    /// rather than recomputed because it is the set as of the moment the
    /// declaration was defined — before the family's own stored members were
    /// merged into the struct's fields, which is what keeps an inherited
    /// default from answering an obligation the declaration owes.
    pub(crate) members: HashSet<String>,
    /// The method names the declaration wrote in its own body.
    ///
    /// A declaration that overrides every family method consumes nothing the
    /// family would have consumed, so it owes the family's requirements
    /// nothing.
    pub(crate) own_methods: HashSet<String>,
    /// Child slots filled from construction trailing content.
    pub(crate) slots: Vec<ContentSlot>,
    /// The heterogeneous family variants this concrete struct wraps into: the
    /// family it is backed by first, then one per family that family extends.
    ///
    /// A declaration is a variant of every family it can be seen through, and
    /// each holds its own tag, so a value coerced to `Any Parent` carries the
    /// tag the parent's dispatchers branch on rather than the child's.
    pub(crate) families: Vec<(EnumId, u32)>,
}

/// One child slot of a construct-backed declaration.
#[derive(Debug, Clone)]
pub(crate) struct ContentSlot {
    /// The slot field's index in the struct's fields.
    pub(crate) field_index: u32,
    /// The slot field's name (its channel name).
    pub(crate) name: String,
    /// Whether the slot holds an ordered list rather than exactly one child.
    pub(crate) list: bool,
    /// The element type each child must satisfy.
    pub(crate) element_ty: Type,
    /// The slot field's stored type.
    pub(crate) field_ty: Type,
    /// Whether the slot declared a default, which a construction that fills
    /// neither the slot nor its position falls back to.
    pub(crate) has_default: bool,
}

/// One concrete variant of a construct family.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ConstructVariant {
    /// The concrete construct-backed struct.
    pub(crate) struct_id: StructId,
    /// Its declaration-order tag in the synthesized family enum.
    pub(crate) tag: u32,
}

/// One method exposed by a construct family.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyMethod<'a> {
    /// The family declaration's method syntax, reused for inherited bodies.
    pub(crate) function: &'a Function,
    /// The file the family declaration belongs to.
    pub(crate) source: SourceId,
    /// Whether reads use property syntax rather than a call.
    pub(crate) computed: bool,
    /// Whether the family declared this as `@Required function f(…)`: a
    /// signature with **no body**, so there is nothing for a backed declaration
    /// to inherit and every variant must implement it itself.
    pub(crate) required: bool,
    /// Whether the declaration wrote a result type.
    ///
    /// A [`required`](Self::required) member that wrote none states an
    /// obligation whose *result* is unconstrained — there is no body to make it
    /// `Void` — so conformance compares parameters only, and a call through the
    /// family value is refused because the family cannot say what it returns.
    ///
    /// A family that wants to say "each declaration decides, and I still want to
    /// name the result" writes `-> Any` instead, which constrains the member and
    /// types the call.
    pub(crate) result_declared: bool,
    /// Resolved written parameters, excluding the receiver.
    pub(crate) params: Vec<Type>,
    /// Parameter ownership modes, aligned with [`Self::params`].
    pub(crate) ownership: Vec<OwnershipMode>,
    /// Resolved result type.
    pub(crate) result: Type,
    /// Whether this is a **uniform** modifier from an `extend` block: one shared
    /// body whose receiver is the family value, rather than a per-variant method
    /// every concrete declaration implements. A uniform method is never
    /// conformance-checked against the variants and is called directly, so
    /// [`Self::dispatcher`] holds its single body rather than a tag dispatcher.
    pub(crate) uniform: bool,
    /// For a per-variant method, the synthesized dynamic dispatcher (reserved on
    /// first use). For a uniform `extend` modifier, its single body (reserved up
    /// front so an uncalled modifier is still checked and lowered).
    pub(crate) dispatcher: Option<FuncId>,
    /// Resolved parameter defaults, aligned with [`Self::params`] (no receiver).
    ///
    /// A `None` slot has no default; a `Some` is the shared HIR that a call
    /// omitting that argument fills with. Empty until resolved after signatures
    /// exist — a family method carries no [`FuncId`] signature row of its own,
    /// so its defaults live here rather than in `param_defaults`.
    pub(crate) defaults: Vec<Option<HirExprId>>,
}

impl ConstructFamilyMethod<'_> {
    /// The result type every implementation must present, or `None` when the
    /// family placed no constraint on it.
    ///
    /// Only a bodyless `@Required function` written without `-> T` is
    /// unconstrained: a member with a body has a real result, and a requirement
    /// that wrote one means it.
    pub(crate) fn constrained_result(&self) -> Option<Type> {
        (!self.required || self.result_declared).then_some(self.result)
    }
}

/// One value member a construct family requires: a `@Required let name: T`.
///
/// Separate from [`ConstructFamilyMethod`] because a field requirement has no
/// AST function behind it. The family states an obligation to *present a value*;
/// a backed declaration discharges it with either a stored field or a computed
/// member, and a read through the family value dispatches to whichever that
/// declaration chose.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyField {
    /// The type every backed declaration must present for this member.
    ///
    /// [`Type::Error`] until [`Analyzer::resolve_family_field_members`] fills
    /// it: the written type is resolved after the family enums exist, because a
    /// requirement may name the family itself.
    ///
    /// [`Analyzer::resolve_family_field_members`]: crate::analyze::Analyzer
    pub(crate) result: Type,
    /// The synthesized dynamic dispatcher, reserved on first read.
    pub(crate) dispatcher: Option<FuncId>,
}

/// A non-required stored field declared by a construct family. Backed
/// declarations inherit these fields and their defaults, which is what makes
/// a family-backed value support sparse component-style updates such as
/// `StyleImplementation { additionalEffect = … }`.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyStoredField {
    pub(crate) name: String,
    pub(crate) ty: Option<TypeRefId>,
    pub(crate) default: Option<ExprId>,
    pub(crate) source: SourceId,
    pub(crate) slot: bool,
}

/// One construct family's type, conformance surface, and concrete variants.
#[derive(Debug, Clone)]
pub(crate) struct ConstructFamilyInfo<'a> {
    /// The synthesized family enum.
    pub(crate) enum_id: EnumId,
    /// Required stored or computed member names.
    pub(crate) required: Vec<String>,
    /// Methods inherited by concrete declarations and dynamically dispatched.
    pub(crate) methods: BTreeMap<String, ConstructFamilyMethod<'a>>,
    /// `@Required let` value members, dynamically dispatched on read.
    pub(crate) field_members: BTreeMap<String, ConstructFamilyField>,
    /// Non-required stored fields inherited by every backed declaration.
    pub(crate) stored_fields: Vec<ConstructFamilyStoredField>,
    /// Concrete backed declarations in source order, including those backed by
    /// a family that extends this one.
    pub(crate) variants: Vec<ConstructVariant>,
    /// Families this one extends, nearest first and already transitive.
    pub(crate) parents: Vec<String>,
    /// The type each declared member was written with, and the file it was
    /// written in.
    ///
    /// Kept as the *written* type rather than a resolved one because this is
    /// read while types are still being resolved: the `name { … }` member
    /// shorthand asks the family what its member returns
    /// (`TypeRef::ConstructMember`), and that question is answered during type
    /// resolution rather than after it. The [`SourceId`] travels with it so the
    /// type resolves against the family's imports, not the shorthand's.
    ///
    /// Covers both member kinds, because both are things a shorthand can
    /// implement: a `@Required let body: Widget` field and a
    /// `@Required function test() -> Any` method.
    pub(crate) member_types: BTreeMap<String, (TypeRefId, SourceId)>,
}

impl crate::analyze::Analyzer<'_> {
    /// The struct a construct-backed declaration named `name` became.
    pub(crate) fn construct_backed_named(&self, name: &str) -> Option<StructId> {
        let id = self.visible_struct(name)?;
        self.constructs.contains_key(&id).then_some(id)
    }

    /// Whether `name` is a computed property of construct-backed `id`.
    pub(crate) fn construct_computed_member(&self, id: StructId, name: &str) -> bool {
        self.constructs
            .get(&id)
            .is_some_and(|info| info.computed.contains(name))
    }
}
