//! The ownership checker: the semantics half of the two enforcement layers.
//!
//! This module owns the `KSEM107`..`KSEM117` band, plus `KSEM250` on a
//! binding's ownership prefix and `KSEM270` on a move a loop repeats. It
//! answers four questions about every binding and every argument:
//!
//! 1. **Is this value still here?** A local that was moved out is gone;
//!    touching it again is `KSEM107`, moving it again is `KSEM110`.
//! 2. **Does this argument transfer, borrow, or copy?** The parameter's
//!    [`OwnershipMode`] decides, and a mismatch between what the parameter
//!    wants and what the call site wrote is `KSEM108`/`KSEM113`/`KSEM114`/
//!    `KSEM115`.
//! 3. **Is this binding allowed to do that?** A `let` cannot be mutably
//!    borrowed (`KSEM109`) and a borrowed parameter cannot be moved onward
//!    (`KSEM111`).
//! 4. **Will this value still be here next time round?** A loop body runs more
//!    than once, so one that gives a value away and does not put it back finds
//!    nothing to give on the next iteration ([`LoopMoves`], `KSEM270`).
//!
//! ## Why only one mode reaches a backend
//!
//! For the type lattice as it stands — scalars, `String`, and structs — a
//! `move` and a `borrow` are both **observationally identical to the deep copy
//! the VM already performs**. Reading a local copies it; a struct's fields are
//! copied with it; the callee drops its own copy at frame exit. A caller that
//! moved a value can never look at it again until it assigns the binding a new
//! one (that is exactly what the checker guarantees), so it cannot tell whether
//! the callee aliased or copied. A
//! `borrow` parameter is read-only, so the callee cannot write anything back
//! for the caller to observe either.
//!
//! So four of the five modes are a static check with **zero** IR, bytecode, VM,
//! LLVM, or wasm change, and it is honest to say so rather than thread an
//! unused mode through fourteen files. `borrow mut` is the one that *is*
//! observable — the callee writes through the caller's binding — and it is
//! carried rather than erased: the parameter joins the callee's by-reference
//! list and every call site records where the write lands (a
//! [`kira_semantics_model::HirWriteback`]), which is the same machinery a
//! mutating method's receiver already used.
//!
//! ## What the aggregates will need
//!
//! Nothing. Arrays alias where structs copy, so an array binding must consume
//! its source — but that rule is already written, as
//! [`kira_semantics_model::Type::moves_on_bind`], and already consulted by
//! [`Analyzer::apply_binding_move`]. Every type answers `false` today, so the
//! path is exercised but never fires. When `Type::Array` lands and answers
//! `true`, implicit-move-on-bind, use-after-move, and partial moves all switch
//! on with no new ownership code.

use kira_semantics_model::hir::{HirExpr, HirExprId, LocalId};
use kira_semantics_model::{OwnershipMode, OwnershipOp, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};

/// The ownership state of one local slot, tracked for the length of a body.
///
/// Kept beside [`kira_semantics_model::hir::HirLocal`] rather than inside it:
/// a move is a fact about *analysis*, not about the program, and nothing below
/// the analyzer has any use for it.
#[derive(Debug, Clone)]
pub(crate) struct LocalOwnership {
    /// How the binding holds its value.
    pub(crate) mode: OwnershipMode,
    /// Where the value was moved out, once it has been.
    ///
    /// `Some` is the whole use-after-move check: a local with a move span is
    /// no longer readable. The span is kept rather than a bare `bool` because
    /// it is what a second label would point at once diagnostics here carry
    /// one — the oracle's use-after-move says "ownership moved here" — and
    /// discarding it now would mean re-deriving it then.
    pub(crate) moved: Option<Span>,
    /// Whether a loop has already blamed this local for a move its back edge
    /// would repeat.
    ///
    /// Every loop enclosing the offending one sees the same local go from live
    /// to moved across its own body, so without this one mistake wears one
    /// diagnostic per level of nesting.
    pub(crate) loop_reported: bool,
}

impl LocalOwnership {
    /// A fresh owned binding.
    pub(crate) fn owned() -> Self {
        Self {
            mode: OwnershipMode::Owned,
            moved: None,
            loop_reported: false,
        }
    }

    /// Whether the binding still holds a value.
    pub(crate) fn is_live(&self) -> bool {
        self.moved.is_none()
    }
}

/// The move state across the arms of a branch.
///
/// Arms are alternatives: at most one of them runs. Analyzing them one after
/// another in a single state makes a move in the first arm a use-after-move in
/// the second, which is the bug this exists to stop — the oracle accepts
/// `Metal -> return f(move p)  Vulkan -> return g(move p)`, and so must this.
///
/// So each arm is analyzed from the state at the branch point, and what reaches
/// the code *after* the branch is the union of the arms that can get there. An
/// arm that definitely returns contributes nothing to that union: its move
/// happened on a path that never rejoins.
pub(crate) struct BranchMoves {
    /// The state every arm starts from.
    before: Vec<Option<Span>>,
    /// The union over the arms that fall through, or `None` when none has yet.
    joined: Option<Vec<Option<Span>>>,
}

impl BranchMoves {
    /// Records the state at the branch point, before any arm is analyzed.
    pub(crate) fn start(ctx: &FnCtx) -> Self {
        Self {
            before: ctx.moved_state(),
            joined: None,
        }
    }

    /// Puts the state back to the branch point, for the next arm to start from.
    pub(crate) fn enter_arm(&self, ctx: &mut FnCtx) {
        ctx.reset_moves(&self.before);
    }

    /// Folds an analyzed arm's state into the join, unless it diverges.
    pub(crate) fn leave_arm(&mut self, ctx: &FnCtx, diverges: bool) {
        if diverges {
            return;
        }
        let arm = ctx.moved_state();
        match &mut self.joined {
            Some(joined) => {
                for (slot, moved) in arm.into_iter().enumerate() {
                    match joined.get_mut(slot) {
                        Some(entry) if entry.is_none() => *entry = moved,
                        _ => {}
                    }
                }
            }
            None => self.joined = Some(arm),
        }
    }

    /// Installs the state the code after the branch sees.
    ///
    /// `exhaustive` says the arms cover every case — an `if` with an `else`, a
    /// `switch` with a `default`, a `match` over every variant. When they do
    /// not, there is one more path to the join: the one where no arm ran, which
    /// arrives with the state the branch started in.
    pub(crate) fn finish(self, ctx: &mut FnCtx, exhaustive: bool) {
        let Some(joined) = self.joined else {
            // Every arm diverged. Either the branch is exhaustive and nothing
            // follows it, or the fall-through path is the only one left; both
            // are the state it started in.
            ctx.reset_moves(&self.before);
            return;
        };
        ctx.reset_moves(&joined);
        if !exhaustive {
            ctx.union_moves(&self.before);
        }
    }
}

/// The move state at a loop's head, and the back edge it has to survive.
///
/// A branch's arms are alternatives, so [`BranchMoves`] asks which of them ran.
/// A loop body is the same code *twice*, which asks a different question: a
/// value the body gives away is already gone when the body starts again. So the
/// body is analyzed once, from the state at the head, and what matters is the
/// state it ends in — a local that entered live and leaves moved cannot survive
/// the jump back.
///
/// This is a rule about the back edge rather than a ban on `move` in a loop,
/// and the difference is exactly the idiom that threads an owned value through
/// one: `tree = step(move tree)` ends the body live, because assigning to a
/// binding reinitializes it, so the next iteration finds a value where it left
/// one.
pub(crate) struct LoopMoves {
    /// The state at the head, so a local a *previous* statement gave away is
    /// not blamed on this loop.
    before: Vec<Option<Span>>,
}

impl LoopMoves {
    /// Records the state at the loop's head, before its body is analyzed.
    ///
    /// Taken before the body's own bindings are declared — a `for` variable
    /// included — so every local this can blame is one that outlives the loop.
    /// A local declared inside is fresh on each iteration and its move belongs
    /// to that iteration alone.
    pub(crate) fn start(ctx: &FnCtx) -> Self {
        Self {
            before: ctx.moved_state(),
        }
    }
}

/// What a call-site argument said about ownership, if anything.
///
/// Only `move` and `copy` can be written on an argument; `borrow` is the
/// callee's request, never the caller's offer.
fn written_op(tree_expr: &Expr) -> Option<OwnershipOp> {
    match tree_expr {
        Expr::Ownership { op, .. } => Some(*op),
        _ => None,
    }
}

impl Analyzer<'_> {
    /// The local a syntax expression names, when it names one directly.
    ///
    /// Only a bare identifier counts. This is the whole reason `f(makeMesh())`
    /// needs no `move` while `f(mesh)` does: a temporary has no binding to
    /// consume, so there is nothing for the caller to lose track of and
    /// nothing to say.
    pub(crate) fn named_local(&self, ctx: &FnCtx, id: ExprId) -> Option<LocalId> {
        match self.tree.expr(id) {
            Expr::Name { symbol, .. } => ctx.resolve(self.interner.resolve(*symbol)),
            _ => None,
        }
    }

    /// Rejects a read of a local that has already been moved out (`KSEM107`).
    ///
    /// Returns whether the read is allowed. This is the first of `KSEM107`'s
    /// three messages: the whole value is gone.
    pub(crate) fn check_local_live(&mut self, ctx: &FnCtx, local: LocalId, span: Span) -> bool {
        let state = ctx.ownership_of(local);
        if state.is_live() {
            return true;
        }
        let name = ctx.local_name(local);
        self.emit(
            span,
            "KSEM107",
            format!("`{name}` was moved and is no longer available here."),
        );
        false
    }

    /// Reports every value a loop body gave away and did not put back
    /// (`KSEM270`).
    ///
    /// `exits` says the body always leaves the loop before reaching its back
    /// edge — by returning, or by breaking out of it. The body then runs at
    /// most once, so a move inside it is as sound as one in straight-line code
    /// and nothing here applies.
    ///
    /// The state is left as the body ended it. A local the loop consumed is
    /// still consumed afterwards, so code following the loop reports `KSEM107`
    /// on its own terms rather than inheriting this one's blame.
    pub(crate) fn check_loop_back_edge(
        &mut self,
        ctx: &mut FnCtx,
        loop_moves: LoopMoves,
        exits: bool,
    ) {
        if exits {
            return;
        }
        for (local, span) in ctx.moves_across(&loop_moves.before) {
            let name = ctx.local_name(local);
            self.emit(
                span,
                "KSEM270",
                format!(
                    "`{name}` is moved inside a loop and has no value again before the next \
                     iteration, which would move it a second time. Assign the binding again \
                     (`{name} = …`), or move it after the loop."
                ),
            );
            ctx.mark_loop_move_reported(local);
        }
    }

    /// Analyzes `move e` / `copy e` written as an expression.
    ///
    /// Every value expression handed to a caller is already an owned result:
    /// local and place reads use the backend's value-copy operation, while
    /// literals and constructors produce fresh values. `copy` therefore needs
    /// no separate HIR or bytecode node. Keeping the expression is important,
    /// though: it makes the ownership intent explicit and prevents the
    /// surrounding call from treating a named non-trivial local as a move.
    /// Arrays use the same copy-on-write handle in the VM and native backends,
    /// so their first mutation remains independent without an eager clone.
    ///
    /// `move` of a bare local marks it moved; a `move` of anything else (a
    /// temporary, a field read) is accepted and consumes nothing, because
    /// there is no binding to consume.
    pub(crate) fn analyze_ownership_expr(
        &mut self,
        ctx: &mut FnCtx,
        op: OwnershipOp,
        operand: ExprId,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        match op {
            OwnershipOp::Copy => self.analyze_copy_expr(ctx, operand, span, expected),
            OwnershipOp::Move => self.analyze_move_expr(ctx, operand, span, expected),
        }
    }

    fn analyze_copy_expr(
        &mut self,
        ctx: &mut FnCtx,
        operand: ExprId,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let value = self.analyze_expr_expecting(ctx, operand, expected);
        // `span` is consumed by the ownership dispatch API for symmetry with
        // `move`; the copy itself has no diagnostic path now that every
        // runtime value has a defined copy operation.
        let _ = span;
        value
    }

    fn analyze_move_expr(
        &mut self,
        ctx: &mut FnCtx,
        operand: ExprId,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let Some(local) = self.named_local(ctx, operand) else {
            // `move someCall()` / `move p.x`: nothing is bound, so nothing is
            // consumed. The operand still analyzes normally.
            return self.analyze_expr_expecting(ctx, operand, expected);
        };
        let state = ctx.ownership_of(local).clone();
        let name = ctx.local_name(local);
        if !state.is_live() {
            self.emit(span, "KSEM110", format!("`{name}` was already moved."));
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if state.mode.is_borrow() {
            self.emit(
                span,
                "KSEM111",
                format!("`{name}` is borrowed and cannot be moved by this scope."),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // A boxed `var` is moved out of by reading what its box holds; the box
        // itself stays where it is, because the closures sharing it still name
        // it. Marking the binding moved is what stops the value being read
        // twice, exactly as it does for an unboxed one.
        let read = self.read_local(ctx, local);
        ctx.mark_moved(local, span);
        read
    }

    /// Applies Rust-style implicit move to a `let`/`var` initializer.
    ///
    /// A binding whose value would otherwise **alias** its source consumes
    /// that source. Which types alias is
    /// [`kira_semantics_model::Type::moves_on_bind`]'s answer, not this
    /// function's: today every type says `false` (a struct deep-copies, a
    /// `String` clones its bytes), so this walks and does nothing. Arrays are
    /// the first `true`, and this is the code that will act on it — which is
    /// why it exists now rather than arriving with them.
    pub(crate) fn apply_binding_move(&mut self, ctx: &mut FnCtx, init: ExprId, value: HirExprId) {
        let bound_ty = self.program.expr(value).type_of();
        if !bound_ty.moves_on_bind() {
            return;
        }
        let Some(local) = self.named_local(ctx, init) else {
            return;
        };
        if ctx.ownership_of(local).mode != OwnershipMode::Owned {
            return;
        }
        if !ctx.ownership_of(local).is_live() {
            return;
        }
        ctx.mark_moved(local, self.tree.expr(init).span());
    }

    /// Checks the ownership prefix written on a binding's annotation, returning
    /// the mode the binding actually gets.
    ///
    /// A binding may say how it takes its initializer — `let f: borrow (Int) ->
    /// Void = g` — and the answer decides one thing: whether the initializer is
    /// consumed.
    ///
    /// `borrow` and `copy` say it is not, and are accepted only for a type that
    /// does not move on bind. That is not a restriction looking for a reason: a
    /// type that *does* move on bind aliases its source, so a binding that
    /// borrowed one would have to share storage with it, and no binding here
    /// shares storage — the one shared thing in this runtime is a capture cell,
    /// which no annotation can name — so the binding would hold a snapshot
    /// while reading as a view, and a later write through the source would silently not be
    /// seen. For every other type an owned binding already leaves the source
    /// alone, so `borrow` and `copy` coincide with `Owned` and can be honored
    /// exactly.
    ///
    /// `borrow mut` is refused everywhere for the sharper form of the same
    /// reason: a write *through* the binding has nowhere to land.
    ///
    /// A refused prefix yields [`OwnershipMode::Owned`], so the binding still
    /// declares and the rest of the body checks against a real type.
    pub(crate) fn check_binding_ownership(
        &mut self,
        ownership: OwnershipMode,
        ownership_span: Option<Span>,
        bound_ty: Type,
    ) -> OwnershipMode {
        let Some(span) = ownership_span else {
            return OwnershipMode::Owned;
        };
        let refusal = match ownership {
            OwnershipMode::Owned | OwnershipMode::Move => return ownership,
            OwnershipMode::BorrowRead | OwnershipMode::Copy if !bound_ty.moves_on_bind() => {
                return ownership;
            }
            OwnershipMode::BorrowRead | OwnershipMode::Copy => format!(
                "a `{}` binding of `{}` would have to share storage with what it binds, \
                 because a value of that type aliases its source; no binding here shares \
                 storage, so it would hold a snapshot while reading as a view. Drop the \
                 prefix to take it by value.",
                ownership.spelling(),
                self.type_name(bound_ty)
            ),
            OwnershipMode::BorrowMut => format!(
                "a `borrow mut` binding of `{}` has nowhere to write through to: a mutable \
                 borrow is carried by writing the callee's final value back into the \
                 caller, and a binding has no caller. Declare the parameter `borrow mut` \
                 where the write belongs instead.",
                self.type_name(bound_ty)
            ),
        };
        self.emit(span, "KSEM250", refusal);
        OwnershipMode::Owned
    }

    /// Consumes a binding's initializer because the binding said `move`.
    ///
    /// Unlike [`Analyzer::apply_binding_move`] this does not ask whether the
    /// type aliases: `move` is written, so the source is given away whatever it
    /// holds — the same thing `move` at an argument means.
    pub(crate) fn force_binding_move(&mut self, ctx: &mut FnCtx, init: ExprId) {
        let Some(local) = self.named_local(ctx, init) else {
            return;
        };
        if ctx.ownership_of(local).mode != OwnershipMode::Owned
            || !ctx.ownership_of(local).is_live()
        {
            return;
        }
        ctx.mark_moved(local, self.tree.expr(init).span());
    }

    /// Checks one argument against the mode its parameter declared.
    ///
    /// This is the whole call-site rule set, in the order the modes decide it:
    /// a dead local is rejected first (whatever the parameter wanted), then
    /// each mode states what it accepts.
    pub(crate) fn analyze_call_argument(
        &mut self,
        ctx: &mut FnCtx,
        arg: ExprId,
        expected: Type,
        ownership: OwnershipMode,
        callee: &str,
    ) -> HirExprId {
        let span = self.tree.expr(arg).span();
        let written = written_op(self.tree.expr(arg));
        let named = self.named_local(ctx, arg);

        // A moved-out local is unusable as an argument regardless of what the
        // parameter asked for, so this precedes every mode's own rule.
        if let Some(local) = named
            && !self.check_local_live(ctx, local, span)
        {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        match ownership {
            OwnershipMode::BorrowRead | OwnershipMode::BorrowMut => {
                if written == Some(OwnershipOp::Move) {
                    self.emit(
                        span,
                        "KSEM114",
                        format!("`{callee}` borrows this argument, so `move` is not allowed here."),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                if ownership == OwnershipMode::BorrowMut
                    && let Some(local) = named
                    && !ctx.is_mutable(local)
                {
                    let name = ctx.local_name(local);
                    self.emit(
                        span,
                        "KSEM109",
                        format!("Cannot mutably borrow immutable binding `{name}`."),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                self.analyze_expr_expecting(ctx, arg, Some(expected))
            }
            OwnershipMode::Copy => {
                if written == Some(OwnershipOp::Move) {
                    self.emit(
                        span,
                        "KSEM115",
                        format!("`{callee}` copies this argument, so `move` is not allowed here."),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                let value = self.analyze_expr_expecting(ctx, arg, Some(expected));
                if written.is_none()
                    && let Some(local) = named
                    && !expected.is_trivially_copyable()
                    && !self.program.expr(value).type_of().is_trivially_copyable()
                {
                    let name = ctx.local_name(local);
                    self.emit(
                        span,
                        "KSEM113",
                        format!("Passing `{name}` to `{callee}` copies a non-trivial value."),
                    );
                }
                value
            }
            OwnershipMode::Owned | OwnershipMode::Move => {
                let value = self.analyze_expr_expecting(ctx, arg, Some(expected));
                if written.is_none()
                    && let Some(local) = named
                    && !expected.is_trivially_copyable()
                    && !self.program.expr(value).type_of().is_trivially_copyable()
                {
                    let name = ctx.local_name(local);
                    self.emit(
                        span,
                        "KSEM108",
                        format!("Passing `{name}` to `{callee}` transfers ownership."),
                    );
                }
                value
            }
        }
    }
}
