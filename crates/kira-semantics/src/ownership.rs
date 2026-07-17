//! The ownership checker: the semantics half of the two enforcement layers.
//!
//! This module owns the `KSEM107`..`KSEM117` band. It answers three questions
//! about every binding and every argument:
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
//!
//! ## Why no backend learns any of this
//!
//! For the type lattice as it stands — scalars, `String`, and structs — a
//! `move` and a `borrow` are both **observationally identical to the deep copy
//! the VM already performs**. Reading a local copies it; a struct's fields are
//! copied with it; the callee drops its own copy at frame exit. A caller that
//! moved a value can never look at it again (that is exactly what the checker
//! guarantees), so it cannot tell whether the callee aliased or copied. A
//! `borrow` parameter is read-only, so the callee cannot write anything back
//! for the caller to observe either.
//!
//! That is why this whole subsystem is a static check with **zero** IR,
//! bytecode, VM, LLVM, or wasm change, and why it is honest to say so rather
//! than thread an unused mode through fourteen files. The first mode that
//! *does* become observable is `borrow mut` — a callee writing through the
//! caller's binding — and it is refused here rather than silently miscompiled;
//! see [`Analyzer::reject_borrow_mut`].
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
}

impl LocalOwnership {
    /// A fresh owned binding.
    pub(crate) fn owned() -> Self {
        Self {
            mode: OwnershipMode::Owned,
            moved: None,
        }
    }

    /// Whether the binding still holds a value.
    pub(crate) fn is_live(&self) -> bool {
        self.moved.is_none()
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

    /// Analyzes `move e` / `copy e` written as an expression.
    ///
    /// `copy` of a non-trivial value is `KSEM116` — the language reserves the
    /// spelling but has no clone semantics, and inventing one here would be
    /// inventing language surface. `move` of a bare local marks it moved; a
    /// `move` of anything else (a temporary, a field read) is accepted and
    /// consumes nothing, because there is no binding to consume.
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
        let ty = self.program.expr(value).type_of();
        if ty.is_trivially_copyable() {
            return value;
        }
        self.emit(
            span,
            "KSEM116",
            format!(
                "Kira parsed `copy`, but cloning `{}` is not implemented yet.",
                self.type_name(ty)
            ),
        );
        self.program.exprs.alloc(HirExpr::Error)
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
        let ty = ctx.local_type(local);
        let read = self.program.exprs.alloc(HirExpr::Local { local, ty });
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
