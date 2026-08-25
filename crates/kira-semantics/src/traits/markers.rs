//! `Send` and `Sync`: what a value may do across a thread boundary.
//!
//! Both are compiler-known and derived structurally, the way `Copyable` is: the
//! fact belongs to every type whether or not anyone writes it down, and a
//! written claim is an assertion checked against the type's own members.
//!
//! # What each one says
//!
//! **`Send`** says a value may be *moved* to another thread. Kira's ownership
//! model makes that true of every value whose storage travels with it: a move
//! leaves exactly one holder, so the thread that receives it is the only one
//! that can touch it. It is false only where the *language itself* hands a
//! value a name for storage the engine keeps — a capture cell, whose other
//! holders keep writing through it; a native-state token, which names a store
//! the engine that minted it owns; and a task handle, which indexes an
//! executor's table.
//!
//! **`Sync`** says a value may be *shared* — borrowed by more than one thread
//! at once. That falls straight out of the same model: `borrow` is a shared
//! read and `borrow mut` is exclusive, so concurrent borrows of one value
//! observe storage nothing may write. It is false for everything `Send` is
//! false for, and additionally for a C block: that is uniquely owned foreign
//! storage a `retains:` parameter may hand away, so a second concurrent holder
//! would be a second owner of storage the foreign side may already have freed.
//!
//! So `Sync` implies `Send`, and neither is decided by how an engine accounts
//! for a copy. The VM shares one heap object between the copies of a struct and
//! native copies its bytes; both are engine-internal bookkeeping under one
//! language-level rule, and a rule that read them would say a `Point` cannot
//! cross a thread boundary, which is false.
//!
//! # Function types
//!
//! A function type carries neither. Its representation struct's fields are the
//! captures of *every* closure literal written with that shape, so they are a
//! join over the whole program rather than a fact about one value — and they
//! are not final until every literal has been lifted, which happens after most
//! of analysis. A type that cannot say what its values kept promises nothing
//! about moving them, and answering per closure is not available: the shape is
//! the type, and two closures of one shape are one type.
//!
//! # Pointer words
//!
//! `RawPtr` and `ForeignPtr` are both. Kira never dereferences a pointer word,
//! frees it, or does arithmetic on it — it stores the word and hands it back —
//! so nothing about the storage at the other end is a fact about the Kira
//! value. Whether that storage may be touched from two threads is the foreign
//! library's contract, stated where the call is declared. This is the same
//! reading `Copyable` already takes of a pointer word.

use std::collections::HashSet;

use kira_semantics_model::{EnumId, StructId, StructOrigin, Type};
use kira_source::{SourceId, Span};

use crate::analyze::Analyzer;

/// One of the two compiler-known thread-safety markers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    /// A value that may be moved to another thread.
    Send,
    /// A value that may be borrowed from more than one thread at once.
    Sync,
}

impl Marker {
    /// The marker named by `name`, or `None` when it names neither.
    pub(crate) fn from_name(name: &str) -> Option<Self> {
        match name {
            super::SEND => Some(Marker::Send),
            super::SYNC => Some(Marker::Sync),
            _ => None,
        }
    }

    /// The trait's name, as source spells it.
    fn name(self) -> &'static str {
        match self {
            Marker::Send => super::SEND,
            Marker::Sync => super::SYNC,
        }
    }

    /// What claiming this marker promises, phrased for a diagnostic.
    fn promise(self) -> &'static str {
        match self {
            Marker::Send => "may be moved to another thread",
            Marker::Sync => "may be borrowed from more than one thread at once",
        }
    }
}

/// What holds the leaf that refutes a marker.
enum Carrier {
    /// The type asked about is itself the leaf.
    Itself,
    /// A named field or variant payload.
    Member(String),
}

/// Why a function type carries neither marker.
const FUNCTION_TYPE: &str =
    "is a function type, and a function type says nothing about what a closure of it captured";

/// The member that refutes a marker claim, and the type that made it fail.
struct NotMarked {
    /// The type carrying the leaf.
    owner: String,
    /// What in it carries the leaf.
    carrier: Carrier,
    /// The leaf type that is not marked, as a user sees it.
    ty: String,
    /// What that leaf is, phrased as the clause the diagnostic reads.
    why: &'static str,
}

impl Analyzer<'_> {
    /// Checks a `Send` or `Sync` claim against the type's own members.
    pub(crate) fn check_marker_claim(
        &mut self,
        marker: Marker,
        ty: StructId,
        source: SourceId,
        span: Span,
    ) {
        self.source = source;
        let name = self.program.types.type_name(Type::Struct(ty));
        let Some(reason) = self.marker_reason(&name, Type::Struct(ty), marker) else {
            return;
        };
        self.emit(
            span,
            "KSEM311",
            format!(
                "`{name}` claims `{}`, but {reason}, so no value of `{name}` {}",
                marker.name(),
                marker.promise()
            ),
        );
    }

    /// Why `ty` does not carry `marker`, phrased as a clause naming the member
    /// that refutes it, or `None` when it does carry it.
    ///
    /// `claimed` is the type the assertion was written on, so a member of that
    /// type reads as "its member" and one reached through another type names
    /// the type it belongs to — which is where the fix goes.
    pub(crate) fn marker_reason(&self, claimed: &str, ty: Type, marker: Marker) -> Option<String> {
        let mut seen = HashSet::new();
        let NotMarked {
            owner,
            carrier,
            ty,
            why,
        } = self.not_marked(ty, marker, &mut seen)?;
        Some(match carrier {
            Carrier::Itself => format!("it {why}"),
            Carrier::Member(member) if owner == claimed => {
                format!("its member `{member}` has type `{ty}`, which {why}")
            }
            Carrier::Member(member) => {
                format!("`{owner}`'s member `{member}` has type `{ty}`, which {why}")
            }
        })
    }

    /// The first member of `ty` that refutes `marker`, or `None` when none
    /// does.
    ///
    /// `seen` breaks a recursive shape: one already being examined cannot be
    /// what refutes itself, so it contributes nothing rather than looping.
    fn not_marked(&self, ty: Type, marker: Marker, seen: &mut HashSet<Type>) -> Option<NotMarked> {
        if let Some(why) = self.leaf_refusal(ty, marker) {
            let name = self.type_name(ty);
            return Some(NotMarked {
                owner: name.clone(),
                carrier: Carrier::Itself,
                ty: name,
                why,
            });
        }
        match ty {
            Type::Struct(id) => self.struct_not_marked(id, marker, seen),
            Type::Enum(id) => self.enum_not_marked(id, marker, seen),
            Type::Array(id) => {
                let element = self.program.types.arrays().element(id)?;
                self.not_marked(element, marker, seen)
            }
            _ => None,
        }
    }

    /// The first field of `id` that refutes `marker`.
    fn struct_not_marked(
        &self,
        id: StructId,
        marker: Marker,
        seen: &mut HashSet<Type>,
    ) -> Option<NotMarked> {
        if !seen.insert(Type::Struct(id)) {
            return None;
        }
        let def = self.program.types.structs().get(id)?;
        let owner = def.name.clone();
        for field in &def.fields {
            let carrier = Carrier::Member(field.name.clone());
            if let Some(reason) = self.member_not_marked(&owner, carrier, field.ty, marker, seen) {
                return Some(reason);
            }
        }
        None
    }

    /// The first variant payload of `id` that refutes `marker`.
    fn enum_not_marked(
        &self,
        id: EnumId,
        marker: Marker,
        seen: &mut HashSet<Type>,
    ) -> Option<NotMarked> {
        if !seen.insert(Type::Enum(id)) {
            return None;
        }
        let def = self.program.types.enums().get(id)?;
        let owner = def.name.clone();
        for variant in &def.variants {
            let Some(payload) = variant.payload else {
                continue;
            };
            let carrier = Carrier::Member(variant.name.clone());
            if let Some(reason) = self.member_not_marked(&owner, carrier, payload, marker, seen) {
                return Some(reason);
            }
        }
        None
    }

    /// Whether one member refutes `marker` for its owner, and which type did.
    fn member_not_marked(
        &self,
        owner: &str,
        carrier: Carrier,
        ty: Type,
        marker: Marker,
        seen: &mut HashSet<Type>,
    ) -> Option<NotMarked> {
        if let Some(why) = self.leaf_refusal(ty, marker) {
            return Some(NotMarked {
                owner: owner.to_owned(),
                carrier,
                ty: self.type_name(ty),
                why,
            });
        }
        // An aggregate carries a marker exactly when everything it holds does,
        // so the answer comes from inside it and the *inner* member is what the
        // diagnostic names. An array is its element for this question.
        match ty {
            Type::Struct(_) | Type::Enum(_) | Type::Array(_) => self.not_marked(ty, marker, seen),
            _ => None,
        }
    }

    /// Why `ty` refutes `marker` on its own, or `None` when it does not.
    ///
    /// An aggregate answers `None` here and is then walked; the types that answer
    /// with a clause are the leaves that settle the question by themselves.
    ///
    /// A function type is one of those leaves rather than an aggregate, even though
    /// its representation *is* a struct. Its fields are the captures of every
    /// closure literal written with that shape, so they are the join over the whole
    /// program rather than a property of any one value — and they are not final
    /// until every literal has been lifted, which is after most of analysis. A type
    /// that cannot promise what its values kept promises neither marker.
    fn leaf_refusal(&self, ty: Type, marker: Marker) -> Option<&'static str> {
        if let Type::Struct(id) = ty
            && self.program.types.structs().origin(id) == StructOrigin::FunctionType
        {
            return Some(FUNCTION_TYPE);
        }
        match ty {
            // The language's shared mutable box: every other holder keeps writing
            // through it, and neither engine takes a lock to do so.
            Type::Cell(_) => Some(
                "is a shared box every holder writes through: it is where a captured `var` lives",
            ),
            // A name for a store the engine that minted the token owns. Carried
            // anywhere that engine is not, it is a number.
            Type::NativeState(_) => Some(
                "is a handle to a store the engine that minted it owns, and names nothing outside it",
            ),
            // A row index in an executor's table, which is per-thread on native and
            // per-instance on the VM.
            Type::Task(_) => Some("is a row in the executor's table, which no other thread has"),
            // Uniquely owned foreign storage a `retains:` parameter may hand away.
            // One owner may move it; a second concurrent holder would be a second
            // owner of storage the foreign side may already have freed.
            Type::CBlock if marker == Marker::Sync => Some(
                "is uniquely owned C storage, so a second holder would be a second owner of storage \
             the foreign side may already have freed",
            ),
            _ => None,
        }
    }
}
