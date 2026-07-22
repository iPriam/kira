//! Per-function analysis state: the local table and the lexical scope stack.
//!
//! Split out of [`super`] on the file-size ladder. It is a cohesive unit with
//! one job — deciding what a name means inside one function body — and the
//! analyzer proper only ever reaches it through the handful of methods here.
//!
//! A frame can be *nested*: while a closure body is analyzed, the enclosing
//! frame moves into [`FnCtx::enclosing`] and moves back out after. That is what
//! lets a name resolve outward through any depth of nesting while every frame
//! still reaches its own state through one `&mut`.

use std::collections::HashMap;

use kira_semantics_model::hir::{HirLocal, HirStmtId, LocalId};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::Span;

use crate::ownership::LocalOwnership;

/// Per-function analysis state: the growing local table and the lexical scope
/// stack mapping names to slots.
pub(crate) struct FnCtx {
    pub(crate) locals: Vec<HirLocal>,
    /// Ownership state per local, positionally aligned with `locals`.
    ///
    /// The two vectors are kept in step by construction: `declare` and
    /// `declare_hidden` are the only ways to add a local and both push to
    /// both, so `ownership[i]` always describes `locals[i]`.
    ownership: Vec<LocalOwnership>,
    /// Where each local's name was written, positionally aligned with
    /// `locals`; `None` for a slot the source never named (hidden desugar
    /// locals, the synthetic `self`).
    ///
    /// Kept in step the same way `ownership` is: every declare pushes to all
    /// three vectors. Declaration sites that know their span record it with
    /// [`FnCtx::note_binding_span`] right after declaring, which keeps the
    /// declare signatures — and their many call sites — unchanged.
    binding_spans: Vec<Option<Span>>,
    pub(crate) scopes: Vec<HashMap<String, LocalId>>,
    pub(crate) return_type: Type,
    /// The struct this body is a method of, when it is one.
    ///
    /// A method's body may name a field bare — `return value + step` rather
    /// than `self.step` — so a name that resolves to no local is tried against
    /// this struct's fields before it is called undefined.
    pub(crate) receiver: Option<StructId>,
    /// How many loops enclose the statement being analyzed.
    ///
    /// A `break`/`continue` at depth zero has no loop to act on and is
    /// reported; every one that survives analysis therefore has a target.
    pub(crate) loop_depth: u32,
    /// The frame this one is lexically nested inside, while a closure body is
    /// being analyzed.
    ///
    /// The enclosing frame *moves* in here for the duration and moves back out
    /// after, which is what lets a name resolve outward through any depth of
    /// nesting while every frame still reaches its own state through one
    /// `&mut`.
    pub(crate) enclosing: Option<Box<FnCtx>>,
    /// The closure this frame is the body of, when it is one.
    pub(crate) closure: Option<crate::closures::ClosureCtx>,
    /// Statements produced while analyzing an expression that must run *before*
    /// the statement the expression belongs to.
    ///
    /// A builder content block (`HStack { For(x in xs) { … } }`) fills its child
    /// slot by running a loop, but a construction is an expression and the HIR
    /// has no block-expression — so the loop is emitted here and the statement
    /// driver drains it ahead of the statement whose expression produced it.
    /// Empty except for the brief window between an expression pushing onto it
    /// and [`Analyzer::analyze_stmt`] flushing it.
    pending_stmts: Vec<HirStmtId>,
}

impl FnCtx {
    pub(crate) fn new(return_type: Type) -> Self {
        Self {
            locals: Vec::new(),
            ownership: Vec::new(),
            binding_spans: Vec::new(),
            scopes: vec![HashMap::new()],
            return_type,
            receiver: None,
            loop_depth: 0,
            enclosing: None,
            closure: None,
            pending_stmts: Vec::new(),
        }
    }

    /// Queues a statement to run before the statement currently being analyzed.
    ///
    /// Used by a builder content block to hoist its slot-filling loop ahead of
    /// the statement whose construction expression it fills. See
    /// [`Self::pending_stmts`].
    pub(crate) fn hoist_stmt(&mut self, stmt: HirStmtId) {
        self.pending_stmts.push(stmt);
    }

    /// Takes the hoisted statements queued since the last drain, in order.
    pub(crate) fn take_pending_stmts(&mut self) -> Vec<HirStmtId> {
        std::mem::take(&mut self.pending_stmts)
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declares a new owned local in the innermost scope, returning its slot.
    ///
    /// Every `let`/`var` binding is owned; only a parameter can be anything
    /// else, and it uses [`FnCtx::declare_param`].
    pub(crate) fn declare(&mut self, name: &str, ty: Type, mutable: bool) -> LocalId {
        self.declare_param(name, ty, mutable, OwnershipMode::Owned)
    }

    /// Declares a local with an explicit ownership mode, returning its slot.
    pub(crate) fn declare_param(
        &mut self,
        name: &str,
        ty: Type,
        mutable: bool,
        ownership: OwnershipMode,
    ) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: name.to_owned(),
            ty,
            mutable,
            ownership,
            native_state: None,
        });
        self.ownership.push(LocalOwnership {
            mode: ownership,
            moved: None,
        });
        self.binding_spans.push(None);
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), id);
        }
        id
    }

    /// Records where `local`'s name was written, for go-to-definition.
    pub(crate) fn note_binding_span(&mut self, local: LocalId, span: Span) {
        self.binding_spans[local.0 as usize] = Some(span);
    }

    /// Marks a local as a mutable view into opaque native callback state.
    pub(crate) fn mark_native_state(
        &mut self,
        local: LocalId,
        type_id: kira_runtime_abi::NativeStateTypeId,
    ) {
        self.locals[local.0 as usize].native_state = Some(type_id);
    }

    /// Where `local`'s name was written, when the source named it.
    pub(crate) fn binding_span(&self, local: LocalId) -> Option<Span> {
        self.binding_spans[local.0 as usize]
    }

    /// The ownership state of a local slot.
    pub(crate) fn ownership_of(&self, local: LocalId) -> &LocalOwnership {
        &self.ownership[local.0 as usize]
    }

    /// Records that `local`'s value was moved out at `span`.
    pub(crate) fn mark_moved(&mut self, local: LocalId, span: Span) {
        self.ownership[local.0 as usize].moved = Some(span);
    }

    /// Snapshots the ownership state so an *effectful* trial analysis can be
    /// rolled back.
    ///
    /// Analyzing a receiver to learn its type also runs its ownership effects —
    /// `move xs` marks `xs` gone. When that analysis is only a probe (the array
    /// path re-resolves the receiver from syntax instead), those effects have to
    /// be undone, or a later use of the receiver reports a move that never
    /// happened. An expression declares no locals, so the state's length is
    /// stable and a whole-vector snapshot restores it exactly.
    pub(crate) fn ownership_snapshot(&self) -> Vec<LocalOwnership> {
        self.ownership.clone()
    }

    /// Restores a snapshot taken by [`FnCtx::ownership_snapshot`].
    pub(crate) fn restore_ownership(&mut self, snapshot: Vec<LocalOwnership>) {
        self.ownership = snapshot;
    }

    /// The name a local was declared with.
    pub(crate) fn local_name(&self, local: LocalId) -> String {
        self.locals[local.0 as usize].name.clone()
    }

    /// Declares a local slot bound to no name, returning it.
    ///
    /// A desugaring needs storage the source never named — a `for` loop's
    /// cursor and limit. Binding it into no scope is what makes it
    /// unreachable: user code cannot read it, write it, or shadow it, whatever
    /// it spells its own variables, because name resolution only ever consults
    /// the scope stack.
    pub(crate) fn declare_hidden(&mut self, ty: Type, mutable: bool) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: String::new(),
            ty,
            mutable,
            ownership: OwnershipMode::Owned,
            native_state: None,
        });
        self.ownership.push(LocalOwnership::owned());
        self.binding_spans.push(None);
        id
    }

    /// Declares a captured binding in the *outermost* scope of a closure body.
    ///
    /// The outermost scope rather than the innermost is what makes a capture
    /// behave like the binding it copies: visible everywhere in the body, and
    /// shadowable by an inner `let` of the same name from that `let` onward.
    pub(crate) fn declare_capture(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: name.to_owned(),
            ty,
            mutable: false,
            ownership: OwnershipMode::Owned,
            native_state: None,
        });
        self.ownership.push(LocalOwnership::owned());
        // A capture stands in for the binding it copies, so a jump from a use
        // of the capture should land where that binding was written; the
        // lifting site records it via `note_binding_span`.
        self.binding_spans.push(None);
        if let Some(scope) = self.scopes.first_mut() {
            scope.insert(name.to_owned(), id);
        }
        id
    }

    /// Whether `local` may be reassigned.
    pub(crate) fn is_mutable(&self, local: LocalId) -> bool {
        self.locals[local.0 as usize].mutable
    }

    /// Resolves a name to a local slot, searching innermost scope outward.
    pub(crate) fn resolve(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(crate) fn local_type(&self, local: LocalId) -> Type {
        self.locals[local.0 as usize].ty
    }
}
