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
use kira_semantics_model::{HirExpr, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ExprId, ForIterable, Stmt, StmtId, SwitchCase};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

mod attempts;
mod fors;
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
    /// Appends rather than returns because one statement of syntax is not
    /// always one statement of HIR: a `for` becomes a prologue plus a loop, and
    /// an unparseable statement becomes nothing at all.
    fn analyze_stmt(&mut self, ctx: &mut FnCtx, stmt_id: StmtId, out: &mut Vec<HirStmtId>) {
        match self.tree.stmt(stmt_id).clone() {
            Stmt::Let {
                name,
                name_span,
                mutable,
                ty,
                init,
                ..
            } => {
                // The annotation is resolved *first* so it can be the
                // initializer's expected type: `var xs: [Int] = []` is the
                // universal empty-array idiom, and `[]` has no element to infer
                // one from — the annotation is the only thing that knows.
                let declared = ty.map(|type_ref| self.resolve_type_ref(type_ref));
                let value = self.analyze_expr_expecting(ctx, init, declared);
                // Rust-style implicit move on bind: a binding whose value would
                // otherwise alias its source consumes that source. An array is
                // the type this fires for.
                self.apply_binding_move(ctx, init, value);
                let value_ty = self.program.expr(value).type_of();
                let local_ty = match declared {
                    Some(annotation) => {
                        if !value_ty.assignable_to(annotation) {
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
                let local = ctx.declare(&name, local_ty, mutable);
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
                if !value_ty.assignable_to(place_ty) {
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
                let hir = self.program.stmts.alloc(HirStmt::Assign {
                    place,
                    value: value_expr,
                });
                out.push(hir);
            }
            Stmt::Return { value, span } => {
                // The declared return type is the expected type, so
                // `function f() -> [Int] { return [] }` knows what `[]` holds.
                let expected = ctx.return_type;
                let hir_value =
                    value.map(|expr| self.analyze_expr_expecting(ctx, expr, Some(expected)));
                self.check_return(ctx, hir_value, span);
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
                let then_body = self.analyze_block(ctx, &then_block);
                let else_body = match &else_block {
                    Some(block) => self.analyze_block(ctx, block),
                    None => Vec::new(),
                };
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
                    ForIterable::Each { array } => {
                        self.analyze_for_each(ctx, cursor, array, &body, span, out)
                    }
                }
            }
            Stmt::Switch {
                subject,
                cases,
                default_block,
                ..
            } => self.analyze_switch(ctx, subject, &cases, default_block.as_ref(), out),
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

    /// Desugars a `switch` into the `if`/`else` chain it already means.
    ///
    /// Given `switch s { case a { A } case b { B } default { D } }`:
    ///
    /// ```text
    /// let <subject> = s              // hidden: evaluated once
    /// if <subject> == a { A }
    /// else if <subject> == b { B }
    /// else { D }
    /// ```
    ///
    /// Every rule the language states falls out of that shape rather than
    /// needing to be enforced:
    ///
    /// * **The subject is evaluated once** — it is bound to a hidden local
    ///   before any comparison.
    /// * **Labels are evaluated lazily, in source order** — each label sits in
    ///   the previous arm's `else`, so a label after the matching one is never
    ///   reached.
    /// * **There is no fallthrough** — the arms are alternatives by
    ///   construction.
    /// * **A `switch` with no `default` and no match does nothing** — the chain
    ///   simply ends with no `else`.
    /// * **`break` inside an arm breaks the enclosing loop, not the switch** —
    ///   an `if` does not push loop depth, so a `break` here means what it would
    ///   mean written anywhere else in the same block.
    ///
    /// A label whose type does not match the subject's is reported: `==` is
    /// what compares them, so a label the subject cannot be compared to is the
    /// same error `s == label` would be.
    fn analyze_switch(
        &mut self,
        ctx: &mut FnCtx,
        subject: ExprId,
        cases: &[SwitchCase],
        default_block: Option<&Block>,
        out: &mut Vec<HirStmtId>,
    ) {
        let subject_expr = self.analyze_expr(ctx, subject);
        let subject_ty = self.program.expr(subject_expr).type_of();

        // Hidden, so no arm can name or shadow the subject's storage.
        let slot = ctx.declare_hidden(subject_ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: slot,
            init: subject_expr,
        });
        out.push(bind);

        // Each arm is analyzed in source order so its diagnostics come out in
        // that order, then the chain is assembled from the back — an `else`
        // has to exist before the `if` that points at it.
        let mut arms = Vec::with_capacity(cases.len());
        for case in cases {
            let cond = self.switch_condition(ctx, slot, subject_ty, case.label);
            let body = self.analyze_block(ctx, &case.body);
            arms.push((cond, body));
        }
        let mut chain = match default_block {
            Some(block) => self.analyze_block(ctx, block),
            None => Vec::new(),
        };
        for (cond, body) in arms.into_iter().rev() {
            let hir = self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: body,
                else_body: chain,
            });
            chain = vec![hir];
        }
        out.extend(chain);
    }

    /// Builds one arm's `<subject> == <label>` test.
    ///
    /// Reports a label the subject cannot be compared to, and yields a `false`
    /// condition in that case so the arm is dead rather than ill-typed — one
    /// bad label costs its own arm and nothing else.
    fn switch_condition(
        &mut self,
        ctx: &mut FnCtx,
        slot: LocalId,
        subject_ty: Type,
        label: ExprId,
    ) -> HirExprId {
        let label_span = self.tree.expr(label).span();
        let label_expr = self.analyze_expr(ctx, label);
        let label_ty = self.program.expr(label_expr).type_of();
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: slot,
            ty: subject_ty,
        });
        match crate::operators::equality_op(subject_ty, label_ty) {
            Some(op) => self.program.exprs.alloc(HirExpr::Binary {
                op,
                lhs: read,
                rhs: label_expr,
                ty: Type::Bool,
            }),
            None => {
                if subject_ty != Type::Error && label_ty != Type::Error {
                    self.emit(
                        label_span,
                        "KSEM044",
                        format!(
                            "a `case` label of type `{}` cannot be compared to a subject \
                             of type `{}`",
                            self.type_name(label_ty),
                            self.type_name(subject_ty)
                        ),
                    );
                }
                self.program.exprs.alloc(HirExpr::Bool(false))
            }
        }
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
                } else if !actual.assignable_to(expected) {
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

    fn analyze_condition(&mut self, ctx: &mut FnCtx, expr: ExprId) -> HirExprId {
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
