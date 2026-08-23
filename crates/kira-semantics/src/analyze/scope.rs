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

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use kira_semantics_model::hir::{HirLocal, HirStmtId, LocalId};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::Span;

use crate::ownership::LocalOwnership;

/// Per-function analysis state: the growing local table and the lexical scope
/// stack mapping names to slots.
///
/// Cloneable so a frame can be **tried**: overload resolution needs the types
/// of a call's arguments before it knows which declaration is being called, and
/// analyzing them moves locals and declares temporaries. The trial runs on a
/// copy, which is then dropped — see [`Analyzer::try_argument_types`].
///
/// [`Analyzer::try_argument_types`]: crate::analyze::Analyzer
#[derive(Clone)]
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
    /// The innermost scope index visible to name lookup while an isolated
    /// declaration-owned expression is analyzed. Ordinary blocks leave this
    /// at zero; construct defaults use a barrier so a field initializer cannot
    /// accidentally capture a same-named local from its construction site.
    scope_floor: usize,
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
    /// Statements produced while analyzing a statement that must run *after*
    /// it.
    ///
    /// The store-back half of writing through a capture cell: a place rooted at
    /// a boxed `var` is rewritten to a temporary the write lands in, and the
    /// temporary's final value goes back into the box here. Draining it after
    /// the statement rather than inside each writing construct is what makes
    /// one rule serve an assignment, an `append`, a mutating method, and a
    /// `borrow mut` argument alike.
    deferred_stmts: Vec<HirStmtId>,
    /// Every name a closure literal in this function mentions.
    ///
    /// Computed once from the syntax before the body is analyzed, and shared
    /// with the frames of the closure bodies inside it, because they are part
    /// of the same function's text. A `var` whose name is in here is boxed at
    /// its declaration — see [`crate::closures::captures`] for why the question
    /// is asked by name and answered early.
    closure_mentions: Rc<HashSet<String>>,
    /// The temporary standing in for each cell written through in the statement
    /// being analyzed, cleared when its deferred store-backs are drained.
    ///
    /// One temporary per cell per statement, so two writes through one cell in
    /// one statement stay recognizably the same storage to the aliasing check.
    cell_temps: HashMap<LocalId, LocalId>,
}

impl FnCtx {
    pub(crate) fn new(return_type: Type) -> Self {
        Self {
            locals: Vec::new(),
            ownership: Vec::new(),
            binding_spans: Vec::new(),
            scopes: vec![HashMap::new()],
            scope_floor: 0,
            return_type,
            receiver: None,
            loop_depth: 0,
            enclosing: None,
            closure: None,
            pending_stmts: Vec::new(),
            deferred_stmts: Vec::new(),
            closure_mentions: Rc::new(HashSet::new()),
            cell_temps: HashMap::new(),
        }
    }

    /// Records which names this function's closure literals mention, so a `var`
    /// among them is boxed at its declaration.
    pub(crate) fn set_closure_mentions(&mut self, mentions: Rc<HashSet<String>>) {
        self.closure_mentions = mentions;
    }

    /// The set to hand a nested closure body's frame: the same one, because a
    /// closure body is part of the same function's text.
    pub(crate) fn closure_mentions(&self) -> Rc<HashSet<String>> {
        Rc::clone(&self.closure_mentions)
    }

    /// Whether a mutable binding named `name` must live in a shared box.
    ///
    /// True when a closure in this function mentions the name at all. The
    /// answer over-approximates on purpose: see [`crate::closures::captures`].
    pub(crate) fn must_box(&self, name: &str, mutable: bool) -> bool {
        mutable && self.closure_mentions.contains(name)
    }

    /// Queues a statement to run *after* the statement currently being
    /// analyzed.
    pub(crate) fn defer_stmt(&mut self, stmt: HirStmtId) {
        self.deferred_stmts.push(stmt);
    }

    /// Takes the deferred statements queued since the last drain, in order,
    /// and forgets the temporaries they wrote back.
    ///
    /// The two go together: a temporary standing in for a cell is only good for
    /// the statement that hoisted it, because the statement after it has its
    /// own `CellGet` to do.
    pub(crate) fn take_deferred_stmts(&mut self) -> Vec<HirStmtId> {
        self.cell_temps.clear();
        std::mem::take(&mut self.deferred_stmts)
    }

    /// The temporary standing in for a cell in the statement being analyzed.
    pub(crate) fn cell_temp(&self, cell: LocalId) -> Option<LocalId> {
        self.cell_temps.get(&cell).copied()
    }

    /// Records the temporary standing in for a cell for the rest of this
    /// statement.
    pub(crate) fn note_cell_temp(&mut self, cell: LocalId, temp: LocalId) {
        self.cell_temps.insert(cell, temp);
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

    /// Starts a scope whose lookup cannot fall through into the surrounding
    /// function. Construct member defaults use this to keep declaration-owned
    /// names separate from the caller's locals while still sharing the
    /// function's local and pending-statement arenas.
    pub(crate) fn push_isolated_scope(&mut self) {
        self.scopes.push(HashMap::new());
        self.scope_floor = self.scopes.len() - 1;
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
        if self.scope_floor >= self.scopes.len() {
            self.scope_floor = 0;
        }
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
            loop_reported: false,
            handed_out: false,
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
    /// Records that a native-state handle in `local` has been handed out, so
    /// the end-of-body check stops holding this body responsible for it.
    pub(crate) fn mark_handed_out(&mut self, local: LocalId) {
        if let Some(state) = self.ownership.get_mut(local.0 as usize) {
            state.handed_out = true;
        }
    }

    /// Every owned native-state handle this body still holds, with where it was
    /// bound.
    ///
    /// Owned only: a `borrow`/`borrow mut` parameter names somebody else's
    /// handle, and freeing it is not this body's business. Handles already
    /// moved out, or handed out with `nativeUserData`, are somebody else's too.
    pub(crate) fn unfreed_native_state_handles(&self) -> Vec<(LocalId, Option<Span>)> {
        let mut found = Vec::new();
        for (slot, state) in self.ownership.iter().enumerate() {
            if state.mode != OwnershipMode::Owned || !state.is_live() || state.handed_out {
                continue;
            }
            let Some(local) = self.locals.get(slot) else {
                continue;
            };
            if !matches!(local.ty, Type::NativeState(_)) {
                continue;
            }
            found.push((
                LocalId(slot as u32),
                self.binding_spans.get(slot).copied().flatten(),
            ));
        }
        found
    }

    pub(crate) fn mark_moved(&mut self, local: LocalId, span: Span) {
        self.ownership[local.0 as usize].moved = Some(span);
    }

    /// Records that `local` holds a value again.
    ///
    /// Assigning to a binding reinitializes it, so a local that was moved out
    /// becomes readable again — `tree = step(move tree)` is the loop-shaped way
    /// a program threads an owned value through a sequence of steps, and
    /// leaving it moved would make every later use a use-after-move.
    pub(crate) fn mark_live(&mut self, local: LocalId) {
        self.ownership[local.0 as usize].moved = None;
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

    /// Where each local was moved out, if it has been — the part of the
    /// ownership state that branching has to reason about.
    ///
    /// Only the move column, because it is the only part a branch changes: a
    /// local's [`OwnershipMode`] is fixed at its declaration. Unlike
    /// [`FnCtx::ownership_snapshot`] this is *not* a whole-state replacement —
    /// a branch declares locals of its own, so the state grows while an arm is
    /// analyzed and truncating it back would leave the ownership column shorter
    /// than the local table it indexes.
    pub(crate) fn moved_state(&self) -> Vec<Option<Span>> {
        self.ownership.iter().map(|state| state.moved).collect()
    }

    /// Puts the move column back to `state`, leaving any local declared since
    /// as it is — such a local is out of scope past the branch that declared it.
    pub(crate) fn reset_moves(&mut self, state: &[Option<Span>]) {
        for (slot, moved) in state.iter().enumerate() {
            if let Some(entry) = self.ownership.get_mut(slot) {
                entry.moved = *moved;
            }
        }
    }

    /// Unions `state` into the move column: a local moved on *either* path is
    /// moved after the two rejoin.
    ///
    /// The conservative direction, and the only sound one: the compiler does not
    /// know which arm ran, so a value one of them gave away is one it may no
    /// longer have.
    pub(crate) fn union_moves(&mut self, state: &[Option<Span>]) {
        for (slot, moved) in state.iter().enumerate() {
            if let Some(entry) = self.ownership.get_mut(slot)
                && entry.moved.is_none()
            {
                entry.moved = *moved;
            }
        }
    }

    /// Every local that was live in `state` and is moved now, with where it
    /// went — what a loop's back edge would re-enter empty.
    ///
    /// A local declared since `state` was taken has no slot in it and is
    /// skipped: it is bound afresh on each iteration, so its move is spent
    /// within one.
    pub(crate) fn moves_across(&self, state: &[Option<Span>]) -> Vec<(LocalId, Span)> {
        state
            .iter()
            .enumerate()
            .filter(|(_, was_moved)| was_moved.is_none())
            .filter_map(|(slot, _)| {
                let entry = self.ownership.get(slot)?;
                if entry.loop_reported {
                    return None;
                }
                entry.moved.map(|span| (LocalId(slot as u32), span))
            })
            .collect()
    }

    /// Records that a loop has blamed `local` for a move across its back edge,
    /// so an enclosing loop does not repeat the report.
    pub(crate) fn mark_loop_move_reported(&mut self, local: LocalId) {
        self.ownership[local.0 as usize].loop_reported = true;
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
        self.declare_hidden_as(ty, mutable, OwnershipMode::Owned)
    }

    /// Declares an unnamed local carrying an explicit ownership mode.
    ///
    /// A synthesized *parameter* needs one: the mode of a slot is what tells
    /// lowering to pass it by reference, so a dispatcher forwarding a `borrow
    /// mut` parameter has to declare it as one or the write it forwards lands
    /// in a copy the caller never sees.
    pub(crate) fn declare_hidden_as(
        &mut self,
        ty: Type,
        mutable: bool,
        ownership: OwnershipMode,
    ) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: String::new(),
            ty,
            mutable,
            ownership,
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
    ///
    /// A capture of a **capture cell** is mutable, and it is the only mutable
    /// one. The binding it stands for was declared `var`, and the whole reason
    /// its value moved into a box is that a write here has to reach the frame
    /// that declared it. Every other capture is a copy, so writing to it would
    /// change something no one can see, and it stays immutable.
    pub(crate) fn declare_capture(&mut self, name: &str, ty: Type) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: name.to_owned(),
            ty,
            mutable: matches!(ty, Type::Cell(_)),
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
            .skip(self.scope_floor)
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(crate) fn local_type(&self, local: LocalId) -> Type {
        self.locals[local.0 as usize].ty
    }
}
