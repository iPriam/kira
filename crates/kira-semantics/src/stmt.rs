//! Statement analysis: one [`Stmt`] of syntax into typed [`HirStmt`]s.
//!
//! Split from [`crate::analyze`], which keeps program-, function-, and
//! struct-level orchestration; everything here works inside one function body,
//! against one [`FnCtx`].
//!
//! # Desugaring happens here
//!
//! Three of the language's statements do not exist below this module. A `for`
//! becomes the one loop shape the HIR has ([`HirStmt::While`]), and a `switch`
//! and a `match` both become a chain of [`HirStmt::If`]. So the IR, the
//! bytecode compiler, the VM, the LLVM backend, and the WASM backend never
//! learn any of them exists — and none can disagree with the construct it
//! desugars to on any backend, because by the time a backend sees one, it *is*
//! that construct.
//!
//! `match` is the case that shows where the limit of a desugar sits: the
//! control flow costs nothing, but binding a variant's payload needed one new
//! *expression* ([`kira_semantics_model::HirExpr::EnumPayload`]) that every
//! backend does have to implement. One node beat one statement form in every
//! layer. See [`matches`] for the shape.
//!
//! That is the first thing to try when adding syntax: a construct the HIR can
//! already express costs a rewrite here instead of a node in every layer.

use kira_semantics_model::hir::{HirExprId, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{HirExpr, OwnershipMode, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ExprId, ForIterable, Stmt, StmtId};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

mod attempts;
pub(crate) mod fors;
mod matches;

impl Analyzer<'_> {
    /// Whether a statement list is guaranteed to execute a `return`.
    ///
    /// A list definitely returns when any of its statements does (everything
    /// after that statement is unreachable). An `if` definitely returns only
    /// when *both* arms do; a loop never counts because its body may run zero
    /// times.
    pub(crate) fn body_definitely_returns(&self, stmts: &[HirStmtId]) -> bool {
        stmts.iter().any(|&id| self.stmt_definitely_returns(id))
    }

    fn stmt_definitely_returns(&self, id: HirStmtId) -> bool {
        match self.program.stmt(id) {
            HirStmt::Return { .. } => true,
            HirStmt::If {
                then_body,
                else_body,
                ..
            } => {
                // An empty else (no `else` written) can fall through, and
                // `body_definitely_returns` is false for an empty list.
                self.body_definitely_returns(then_body) && self.body_definitely_returns(else_body)
            }
            _ => false,
        }
    }

    pub(crate) fn analyze_block(&mut self, ctx: &mut FnCtx, block: &Block) -> Vec<HirStmtId> {
        ctx.push_scope();
        let mut stmts = Vec::with_capacity(block.stmts.len());
        self.analyze_stmts(ctx, &block.stmts, &mut stmts);
        ctx.pop_scope();
        stmts
    }

    /// Analyzes each statement of `ids` in order, appending onto `out`.
    fn analyze_stmts(&mut self, ctx: &mut FnCtx, ids: &[StmtId], out: &mut Vec<HirStmtId>) {
        for &stmt_id in ids {
            self.analyze_stmt(ctx, stmt_id, out);
        }
    }

    /// Analyzes one statement, appending what it lowers to onto `out`.
    ///
    /// A statement's expressions may hoist statements of their own — a builder
    /// content block fills its slot by running a loop, and the HIR has no
    /// block-expression to carry it (see [`FnCtx::hoist_stmt`]). Those run
    /// *before* the statement that produced them, so they are drained onto `out`
    /// ahead of it.
    fn analyze_stmt(&mut self, ctx: &mut FnCtx, stmt_id: StmtId, out: &mut Vec<HirStmtId>) {
        let mut produced = Vec::new();
        self.analyze_stmt_inner(ctx, stmt_id, &mut produced);
        out.extend(ctx.take_pending_stmts());
        out.append(&mut produced);
        // A write through a capture cell lands in a temporary and is stored
        // back afterwards — see [`crate::cells`]. The store-back is queued
        // while the statement is analyzed and runs here, after it.
        out.extend(ctx.take_deferred_stmts());
    }

    /// Lowers one statement to HIR, appending what it becomes onto `out`.
    ///
    /// Appends rather than returns because one statement of syntax is not
    /// always one statement of HIR: a `for` becomes a prologue plus a loop, and
    /// an unparseable statement becomes nothing at all.
    fn analyze_stmt_inner(&mut self, ctx: &mut FnCtx, stmt_id: StmtId, out: &mut Vec<HirStmtId>) {
        match self.tree.stmt(stmt_id).clone() {
            Stmt::Let {
                name,
                name_span,
                mutable,
                ty,
                ownership,
                ownership_span,
                init,
                ..
            } => {
                // The annotation is resolved *first* so it can be the
                // initializer's expected type: `var xs: [Int] = []` is the
                // universal empty-array idiom, and `[]` has no element to infer
                // one from — the annotation is the only thing that knows.
                let declared = ty.map(|type_ref| self.resolve_type_ref(type_ref));
                let value = self.analyze_expr_expecting(ctx, init, declared);
                let value_ty = self.program.expr(value).type_of();
                // What the binding does to its initializer is exactly what its
                // ownership prefix decides, so the prefix is checked before the
                // move is applied rather than after.
                let ownership = self.check_binding_ownership(ownership, ownership_span, value_ty);
                match ownership {
                    // Rust-style implicit move on bind: a binding whose value
                    // would otherwise alias its source consumes that source. An
                    // array is the type this fires for.
                    OwnershipMode::Owned => self.apply_binding_move(ctx, init, value),
                    // Written `move`, so the source is given away whatever it
                    // holds.
                    OwnershipMode::Move => self.force_binding_move(ctx, init),
                    // `borrow` and `copy` leave the source alone — and are only
                    // accepted for a type an owned binding would have left alone
                    // too, so there is nothing here to do differently.
                    _ => {}
                }
                let local_ty = match declared {
                    Some(annotation) => {
                        if !self.admits(value_ty, annotation) {
                            self.emit(
                                name_span,
                                "KSEM020",
                                format!(
                                    "binding annotated `{}` cannot hold a value of type `{}`",
                                    self.type_name(annotation),
                                    self.type_name(value_ty)
                                ),
                            );
                        }
                        annotation
                    }
                    None => value_ty,
                };
                let native_state = match self.program.expr(value) {
                    HirExpr::NativeRecover { type_id, .. } => Some(*type_id),
                    _ => None,
                };
                let name = self.interner.resolve(name).to_owned();
                // `let x: Any = 1` stores an erased value, so the crossing
                // happens here rather than being left for a backend to notice.
                let value = self.coerce_into(value, local_ty);
                // A `var` a closure in this function could name moves into a
                // shared box, so the closure and this frame write the same
                // storage. The decision is made here — at the declaration —
                // because every read of the binding below this point has to
                // agree with it, and the capture that needs it is analyzed
                // later. A recovered callback-state view is never boxed: it is
                // a window into storage a host owns, not storage of ours.
                let boxes = !ownership.is_borrow()
                    && native_state.is_none()
                    && ctx.must_box(&name, mutable)
                    && crate::cells::cell_can_hold(local_ty);
                let (value, slot_ty) = if boxes {
                    self.box_binding(value, local_ty)
                } else {
                    (value, local_ty)
                };
                let local = ctx.declare_param(&name, slot_ty, mutable, ownership);
                if let Some(type_id) = native_state {
                    ctx.mark_native_state(local, type_id);
                }
                ctx.note_binding_span(local, name_span);
                let hir = self
                    .program
                    .stmts
                    .alloc(HirStmt::Let { local, init: value });
                out.push(hir);
            }
            Stmt::Assign { target, value, .. } => {
                let target_span = self.tree.expr(target).span();
                // The place is resolved before the value for the same reason
                // the annotation is: it is what supplies `xs = []` an element
                // type. It also fixes evaluation order — a place's index
                // expressions are evaluated before the value, on every backend.
                let Some((place, place_ty)) = self.resolve_place(ctx, target, PlacePurpose::Assign)
                else {
                    self.analyze_expr(ctx, value);
                    return;
                };
                let value_expr = self.analyze_expr_expecting(ctx, value, Some(place_ty));
                let value_ty = self.program.expr(value_expr).type_of();
                if !self.admits(value_ty, place_ty) {
                    self.emit(
                        target_span,
                        "KSEM022",
                        format!(
                            "cannot assign a value of type `{}` to a place of type `{}`",
                            self.type_name(value_ty),
                            self.type_name(place_ty)
                        ),
                    );
                }
                // Assigning to the binding itself gives it a value again. The
                // value is analyzed first, so `x = f(move x)` still reads the
                // live `x` and only then restores it; `x.f = …` writes into a
                // value the binding must already hold, and restores nothing.
                if place.path.is_empty() {
                    ctx.mark_live(place.local);
                }
                let value_expr = self.coerce_into(value_expr, place_ty);
                // Replacing a boxed `var` writes *into the box*, in one step
                // that releases what was there. Nothing else in the HIR does
                // that, which is why it is its own statement rather than an
                // assignment to a slot.
                let hir = if place.path.is_empty() && self.cell_inner(ctx, place.local).is_some() {
                    self.program.stmts.alloc(HirStmt::CellSet {
                        local: place.local,
                        value: value_expr,
                    })
                } else {
                    self.program.stmts.alloc(HirStmt::Assign {
                        place,
                        value: value_expr,
                    })
                };
                out.push(hir);
            }
            Stmt::Return { value, span } => {
                // The declared return type is the expected type, so
                // `function f() -> [Int] { return [] }` knows what `[]` holds.
                let expected = ctx.return_type;
                let hir_value =
                    value.map(|expr| self.analyze_expr_expecting(ctx, expr, Some(expected)));
                self.check_return(ctx, hir_value, span);
                // After the check, so a value that failed it is reported against
                // the type it actually had rather than against `Any`.
                let hir_value = hir_value.map(|expr| self.coerce_into(expr, expected));
                let hir = self
                    .program
                    .stmts
                    .alloc(HirStmt::Return { value: hir_value });
                out.push(hir);
            }
            Stmt::Expr { expr, .. } => {
                let hir = self.analyze_expr(ctx, expr);
                let hir = self.program.stmts.alloc(HirStmt::Expr { expr: hir });
                out.push(hir);
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let cond_expr = self.analyze_condition(ctx, cond);
                // The two arms are alternatives, so each is analyzed from the
                // state at the `if` — a move in one is not a move in the other.
                let mut branch = crate::ownership::BranchMoves::start(ctx);
                let then_body = self.analyze_block(ctx, &then_block);
                branch.leave_arm(ctx, self.body_definitely_returns(&then_body));
                branch.enter_arm(ctx);
                let else_body = match &else_block {
                    Some(block) => self.analyze_block(ctx, block),
                    None => Vec::new(),
                };
                if else_block.is_some() {
                    branch.leave_arm(ctx, self.body_definitely_returns(&else_body));
                }
                branch.finish(ctx, else_block.is_some());
                let hir = self.program.stmts.alloc(HirStmt::If {
                    cond: cond_expr,
                    then_body,
                    else_body,
                });
                out.push(hir);
            }
            Stmt::While { cond, body, .. } => {
                let cond_expr = self.analyze_condition(ctx, cond);
                ctx.loop_depth += 1;
                let loop_body = self.analyze_block(ctx, &body);
                ctx.loop_depth -= 1;
                let hir = self.program.stmts.alloc(HirStmt::While {
                    cond: cond_expr,
                    body: loop_body,
                });
                out.push(hir);
            }
            Stmt::For {
                name,
                name_span,
                iterable,
                body,
                span,
                ..
            } => {
                let cursor = fors::ForCursor {
                    name,
                    span: name_span,
                };
                match iterable {
                    ForIterable::Range { start, end } => {
                        self.analyze_for_range(ctx, cursor, (start, end), &body, out)
                    }
                    ForIterable::Each { array } => self.analyze_for_each(
                        ctx,
                        cursor,
                        array,
                        span,
                        out,
                        |analyzer, ctx, out| {
                            analyzer.analyze_stmts(ctx, &body.stmts, out);
                        },
                    ),
                }
            }
            Stmt::Match {
                subject,
                arms,
                span,
            } => self.analyze_match(ctx, subject, &arms, span, out),
            Stmt::Attempt {
                body,
                handlers,
                span,
            } => self.analyze_attempt(ctx, &body, &handlers, span, out),
            Stmt::Break { span } => {
                if self.check_in_loop(ctx, span, "break", "KSEM041") {
                    let hir = self.program.stmts.alloc(HirStmt::Break);
                    out.push(hir);
                }
            }
            Stmt::Continue { span } => {
                if self.check_in_loop(ctx, span, "continue", "KSEM042") {
                    let hir = self.program.stmts.alloc(HirStmt::Continue);
                    out.push(hir);
                }
            }
            Stmt::Error { .. } => {}
        }
    }

    /// Reports a `break`/`continue` written outside any loop.
    ///
    /// Returning `false` drops the statement instead of emitting it, which is
    /// what lets every backend assume a jump target exists.
    fn check_in_loop(
        &mut self,
        ctx: &FnCtx,
        span: Span,
        keyword: &str,
        code: &'static str,
    ) -> bool {
        if ctx.loop_depth == 0 {
            self.emit(
                span,
                code,
                format!("`{keyword}` outside of a loop has nothing to {keyword}"),
            );
            return false;
        }
        true
    }

    /// Allocates a read of an `Int`-typed local, for a desugaring building one.
    fn read_int_local(&mut self, local: LocalId) -> HirExprId {
        self.program.exprs.alloc(HirExpr::Local {
            local,
            ty: Type::INT,
        })
    }

    /// Analyzes one `for` range bound, requiring an integer.
    ///
    /// Any integer *spelling* qualifies — `for i in 0..u8Count` is legal, not
    /// just a bare `Int`. This is a kind check, deliberately not an exact-type
    /// one: a bound is counted with, never stored, so its width is irrelevant
    /// to the loop.
    fn analyze_bound(&mut self, ctx: &mut FnCtx, expr: ExprId) -> HirExprId {
        let span = self.tree.expr(expr).span();
        let hir = self.analyze_expr(ctx, expr);
        let ty = self.program.expr(hir).type_of();
        if !matches!(ty, Type::Int(_)) && ty != Type::Error {
            self.emit(
                span,
                "KSEM043",
                format!(
                    "a `for` range bound must be `Int`, found `{}`",
                    self.type_name(ty)
                ),
            );
        }
        hir
    }

    fn check_return(&mut self, ctx: &FnCtx, value: Option<HirExprId>, span: Span) {
        let expected = ctx.return_type;
        match value {
            None => {
                if expected != Type::Void {
                    self.emit(
                        span,
                        "KSEM030",
                        format!(
                            "function must return a value of type `{}`",
                            self.type_name(expected)
                        ),
                    );
                }
            }
            Some(expr) => {
                let actual = self.program.expr(expr).type_of();
                if expected == Type::Void {
                    self.emit(span, "KSEM031", "a `Void` function cannot return a value");
                } else if !self.admits(actual, expected) {
                    self.emit(
                        span,
                        "KSEM032",
                        format!(
                            "returning `{}` from a function declared to return `{}`",
                            self.type_name(actual),
                            self.type_name(expected)
                        ),
                    );
                }
            }
        }
    }

    pub(crate) fn analyze_condition(&mut self, ctx: &mut FnCtx, expr: ExprId) -> HirExprId {
        let cond_span = self.tree.expr(expr).span();
        let hir = self.analyze_expr(ctx, expr);
        let ty = self.program.expr(hir).type_of();
        if ty != Type::Bool && ty != Type::Error {
            self.emit(
                cond_span,
                "KSEM040",
                format!("condition must be `Bool`, found `{}`", self.type_name(ty)),
            );
        }
        hir
    }
}
