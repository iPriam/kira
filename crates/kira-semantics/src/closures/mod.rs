//! Closures, resolved entirely in the frontend.
//!
//! A closure adds **no** IR node, **no** opcode, and **no** backend code. The
//! whole feature is a desugar, in the same spirit as classes: a function type
//! becomes a synthesized *representation struct*, a closure literal becomes a
//! lifted top-level function plus a value of that struct, and calling a
//! closure-typed value becomes a call to a synthesized *dispatcher* that
//! switches on the struct's tag.
//!
//! # Why a struct and not a function pointer
//!
//! A function pointer would need an indirect call, which means a call table in
//! wasm, a pointer type in LLVM, and a new opcode in the VM — the full
//! horizontal slice for one feature. Defunctionalizing costs none of that: the
//! set of closure literals in a program is finite and known once analysis
//! finishes, so a call through a closure value is a branch over that set. Every
//! construct the desugar emits — a struct, a field read, an `if`, a call — is
//! one every backend already runs.
//!
//! # The representation
//!
//! For each distinct function type the program mentions, one struct:
//!
//! ```text
//! struct `(Int) -> Void` { tag: Int, <captures of closure #0…>, <#1…>, … }
//! ```
//!
//! Field 0 is the tag — which closure literal this value is. The remaining
//! fields are the concatenation of every literal's captures, so two literals of
//! the same type never share a slot and a value only ever fills its own. The
//! fields grow as literals are found, which is why a literal's `StructNew` is
//! *finalized* after all bodies are analyzed rather than built complete.
//!
//! # What is refused, and why
//!
//! A capture must be an immutable binding of a trivially-copyable type. The
//! oracle borrows a mutable local instead of copying it, which needs shared
//! mutable storage; nothing in this runtime has reference semantics yet — every
//! value is copied on read, on every backend — so a `var` capture is refused
//! (`KSEM117`) rather than silently copied, which would run and give the wrong
//! answer. A `String`, struct, array, or enum capture is refused by the same
//! code, matching the oracle: `isTriviallyCopyable` admits only the scalars.

use std::collections::HashMap;

use kira_semantics_model::hir::{FuncId, LocalId};
use kira_semantics_model::{OwnershipMode, StructId, Type};

mod calls;
mod lift;

/// One function type, and everything synthesized for it.
#[derive(Debug, Clone)]
pub(crate) struct FnTypeInfo {
    /// The parameter types, in order.
    pub(crate) params: Vec<Type>,
    /// The ownership mode of each parameter, index-aligned with `params`.
    ///
    /// What an indirect call checks its arguments against: a `borrow`
    /// parameter takes no `move`, an owned one does.
    pub(crate) param_ownership: Vec<OwnershipMode>,
    /// The result type.
    pub(crate) result: Type,
    /// The dispatcher's id, minted on the first call *through* a value of this
    /// type. A type that is only ever constructed and never called needs none.
    pub(crate) dispatcher: Option<FuncId>,
    /// One entry per closure literal or named function of this type, indexed by tag.
    pub(crate) impls: Vec<ClosureImpl>,
    /// The tag already assigned to each named function reference of this type.
    pub(crate) named_functions: HashMap<FuncId, u32>,
}

/// One closure literal, lifted to a top-level function.
#[derive(Debug, Clone)]
pub(crate) struct ClosureImpl {
    /// The lifted function's id.
    pub(crate) function: FuncId,
}

/// A closure literal's `StructNew`, waiting for the field list to stop growing.
#[derive(Debug, Clone)]
pub(crate) struct ClosureSite {
    /// The `HirExpr::StructNew` node to finalize.
    pub(crate) expr: kira_semantics_model::hir::HirExprId,
    /// The representation struct.
    pub(crate) repr: StructId,
    /// Which literal of that type this is.
    pub(crate) tag: u32,
    /// The field each captured value belongs in.
    pub(crate) capture_fields: Vec<u32>,
    /// The captured values, read in the *enclosing* frame, aligned with
    /// `capture_fields`.
    pub(crate) capture_values: Vec<kira_semantics_model::hir::HirExprId>,
}

/// The per-closure state a [`FnCtx`](crate::analyze::FnCtx) carries while a
/// lifted body is being analyzed.
#[derive(Debug, Clone)]
pub(crate) struct ClosureCtx {
    /// The representation struct this closure's captures live in.
    pub(crate) repr: StructId,
    /// Which literal of that type this is, so a capture's synthesized field
    /// name is unique across every literal sharing the representation struct.
    pub(crate) tag: u32,
    /// Each capture, in discovery order.
    pub(crate) captures: Vec<Capture>,
}

/// One captured binding, threaded through one frame.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Capture {
    /// The slot the value is read from, in the *enclosing* frame.
    pub(crate) outer: LocalId,
    /// The slot it is bound to inside this closure.
    pub(crate) inner: LocalId,
    /// The representation struct field it travels in.
    pub(crate) field: u32,
}

/// What resolving a name against the enclosing closure frames produced.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Captured {
    /// The name resolves, to this slot in the current frame.
    Local(LocalId),
    /// The name resolves in an enclosing frame but may not be captured; a
    /// diagnostic was already emitted.
    Refused,
    /// The name resolves in no frame at all.
    Absent,
}

/// Whether a value of `ty` may be copied into a closure without a `copy`.
///
/// The oracle's `isTriviallyCopyable`: the scalars, and a `RawPtr`, which is an
/// opaque word that copies bits and frees nothing. A `String`, a struct, an
/// array, and an enum all own heap storage, and copying one into a closure is
/// exactly the "non-Copy owned capture" `KSEM117` names.
///
/// A **function type** is decided by [`Analyzer::capture_is_trivially_copyable`]
/// instead, because answering it needs the function-type table this cannot see.
pub(crate) fn is_trivially_copyable(ty: Type) -> bool {
    matches!(
        ty,
        Type::Int(_) | Type::Float(_) | Type::Bool | Type::Void | Type::Error | Type::RawPtr
    )
}

impl crate::analyze::Analyzer<'_> {
    /// Whether a capture of `ty` copies without owning anything.
    ///
    /// [`is_trivially_copyable`] plus the case it cannot see: a **function
    /// value**. Its representation is a tag and this program's captures of that
    /// type, and every one of those had to pass this same test to become a
    /// capture at all — so a function value owns nothing transitively, and
    /// copying one into a closure copies words.
    ///
    /// That is what lets a callback be threaded through a closure, which is how
    /// an application hands a frame handler to a loop it builds inline.
    pub(crate) fn capture_is_trivially_copyable(&self, ty: Type) -> bool {
        is_trivially_copyable(ty) || self.as_function_type(ty).is_some()
    }

    /// Whether storing a value of `ty` in `repr` would let `repr` reach itself.
    ///
    /// Only a function value can raise the question: every other capturable type
    /// is a scalar. Two function types may nest freely — a `(Int) -> Int` value
    /// captured by a `() -> Void` closure is one struct holding another — and it
    /// is exactly the *cycle* that has no representation, because the struct
    /// would contain a value of its own type.
    pub(crate) fn capture_would_be_cyclic(&self, repr: StructId, ty: Type) -> bool {
        let mut seen = std::collections::HashSet::new();
        self.type_reaches_struct(ty, repr, &mut seen)
    }

    /// Whether `ty` contains a value of `target` by value, at any depth.
    fn type_reaches_struct(
        &self,
        ty: Type,
        target: StructId,
        seen: &mut std::collections::HashSet<StructId>,
    ) -> bool {
        let Type::Struct(id) = ty else {
            return false;
        };
        if id == target {
            return true;
        }
        if !seen.insert(id) {
            return false;
        }
        let field_types: Vec<Type> = match self.program.types.structs().get(id) {
            Some(def) => def.fields.iter().map(|field| field.ty).collect(),
            None => return false,
        };
        field_types
            .into_iter()
            .any(|field| self.type_reaches_struct(field, target, seen))
    }
}

/// The interning key for a function type: its parameters, their ownership
/// modes, and its result.
///
/// The modes are part of the key because they are part of the type: a call
/// through `(borrow Event) -> Void` leaves the caller its value and a call
/// through `(Event) -> Void` takes it, so the two cannot share a row.
pub(crate) type FnTypeKey = (Vec<Type>, Vec<OwnershipMode>, Type);

/// Every function type the program mentions, and the struct each became.
#[derive(Debug, Default)]
pub(crate) struct FnTypeTable {
    /// Keyed by the representation struct, which is what a [`Type`] carries.
    by_struct: HashMap<StructId, FnTypeInfo>,
    /// Keyed by shape, so `(Int) -> Void` written twice is one type.
    by_shape: HashMap<FnTypeKey, StructId>,
}

impl FnTypeTable {
    /// The function type behind a struct id, or `None` when the struct is an
    /// ordinary one the user declared.
    pub(crate) fn get(&self, id: StructId) -> Option<&FnTypeInfo> {
        self.by_struct.get(&id)
    }

    /// The function type behind a struct id, mutably.
    pub(crate) fn get_mut(&mut self, id: StructId) -> Option<&mut FnTypeInfo> {
        self.by_struct.get_mut(&id)
    }

    /// The struct already minted for `key`, if any.
    pub(crate) fn lookup(&self, key: &FnTypeKey) -> Option<StructId> {
        self.by_shape.get(key).copied()
    }

    /// Records a freshly minted representation struct.
    pub(crate) fn insert(&mut self, key: FnTypeKey, id: StructId, info: FnTypeInfo) {
        self.by_shape.insert(key, id);
        self.by_struct.insert(id, info);
    }

    /// Every function type, as `(struct, info)`, in no particular order.
    pub(crate) fn rows(&self) -> impl Iterator<Item = (StructId, &FnTypeInfo)> {
        self.by_struct.iter().map(|(&id, info)| (id, info))
    }
}
