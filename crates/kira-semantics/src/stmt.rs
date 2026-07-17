//! Statement analysis: one [`Stmt`] of syntax into typed [`HirStmt`]s.
//!
//! Split from [`crate::analyze`], which keeps program-, function-, and
//! struct-level orchestration; everything here works inside one function body,
//! against one [`FnCtx`].
//!
//! # Desugaring happens here
//!
//! The HIR has one loop shape ([`HirStmt::While`]). A `for` in the source is
//! rewritten into it during analysis, so the IR, the bytecode compiler, the VM,
//! the LLVM backend, and the WASM backend never learn that `for` exists — and a
//! `for` loop cannot disagree with a `while` loop on any backend, because by
//! the time a backend sees one, it *is* one.

use kira_core::Symbol;
use kira_semantics_model::hir::{HirExprId, HirPlace, HirStmt, HirStmtId};
use kira_semantics_model::{HirBinaryOp, HirExpr, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, Expr, ExprId, Stmt, StmtId};

use crate::analyze::{Analyzer, FnCtx};

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
                let value = self.analyze_expr(ctx, init);
                let value_ty = self.program.expr(value).type_of();
                let declared = ty.map(|type_ref| self.resolve_type(type_ref.name, type_ref.span));
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
                let name = self.interner.resolve(name).to_owned();
                let local = ctx.declare(&name, local_ty, mutable);
                let hir = self
                    .program
                    .stmts
                    .alloc(HirStmt::Let { local, init: value });
                out.push(hir);
            }
            Stmt::Assign { target, value, .. } => {
                let value_expr = self.analyze_expr(ctx, value);
                let value_ty = self.program.expr(value_expr).type_of();
                let target_span = self.tree.expr(target).span();
                let Some((place, place_ty)) = self.resolve_place(ctx, target) else {
                    return;
                };
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
                let hir_value = value.map(|expr| self.analyze_expr(ctx, expr));
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
                start,
                end,
                body,
                ..
            } => self.analyze_for(ctx, name, (start, end), &body, out),
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

    /// Desugars `for <name> in <start>..<end> { … }` into a `while`.
    ///
    /// The rewrite, given `for i in a..b { body }`:
    ///
    /// ```text
    /// var  <cursor> = a          // hidden, mutable: the iteration state
    /// let  <limit>  = b          // hidden: evaluated once, not per iteration
    /// while <cursor> < <limit> {
    ///     let i = <cursor>       // the user's variable: a fresh immutable copy
    ///     <cursor> = <cursor> + 1
    ///     body
    /// }
    /// ```
    ///
    /// Two details carry the whole correctness argument:
    ///
    /// * **The increment precedes the body.** Putting it last would let
    ///   `continue` jump over it and spin forever. Stepping the cursor before
    ///   the body runs means every exit from the body — falling off the end,
    ///   `continue`, or `break` — leaves the cursor already advanced.
    /// * **`i` is a fresh immutable copy, not the cursor.** Writing to `i` is
    ///   rejected (it is a `let`), so a body cannot perturb the iteration.
    ///
    /// The bounds are `Int`; a non-`Int` bound is reported and the loop is
    /// still built, so one bad bound does not cascade.
    fn analyze_for(
        &mut self,
        ctx: &mut FnCtx,
        name: Symbol,
        range: (ExprId, ExprId),
        body: &Block,
        out: &mut Vec<HirStmtId>,
    ) {
        let (start, end) = range;
        let start_expr = self.analyze_bound(ctx, start);
        let end_expr = self.analyze_bound(ctx, end);

        // The cursor and limit are hidden: they occupy local slots but are
        // bound to no name, so a body cannot read, write, or shadow them — not
        // even one that declares a variable spelled the same way.
        let cursor = ctx.declare_hidden(Type::Int, true);
        let limit = ctx.declare_hidden(Type::Int, false);
        let cursor_init = self.program.stmts.alloc(HirStmt::Let {
            local: cursor,
            init: start_expr,
        });
        let limit_init = self.program.stmts.alloc(HirStmt::Let {
            local: limit,
            init: end_expr,
        });
        out.push(cursor_init);
        out.push(limit_init);

        // `<cursor> < <limit>` — half-open, so `for i in 5..5` never runs.
        let cursor_read = self.read_int_local(cursor);
        let limit_read = self.read_int_local(limit);
        let cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: cursor_read,
            rhs: limit_read,
            ty: Type::Bool,
        });

        // The user's variable lives in its own scope: it is visible to the
        // body and gone afterwards.
        ctx.push_scope();
        let user_name = self.interner.resolve(name).to_owned();
        let variable = ctx.declare(&user_name, Type::Int, false);
        let cursor_copy = self.read_int_local(cursor);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: variable,
            init: cursor_copy,
        });

        let step_read = self.read_int_local(cursor);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let stepped = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: step_read,
            rhs: one,
            ty: Type::Int,
        });
        let step = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: cursor,
                path: Vec::new(),
            },
            value: stepped,
        });

        let mut loop_body = vec![bind, step];
        ctx.loop_depth += 1;
        ctx.push_scope();
        self.analyze_stmts(ctx, &body.stmts, &mut loop_body);
        ctx.pop_scope();
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let hir = self.program.stmts.alloc(HirStmt::While {
            cond,
            body: loop_body,
        });
        out.push(hir);
    }

    /// Allocates a read of an `Int`-typed local, for a desugaring building one.
    fn read_int_local(&mut self, local: kira_semantics_model::LocalId) -> HirExprId {
        self.program.exprs.alloc(HirExpr::Local {
            local,
            ty: Type::Int,
        })
    }

    /// Analyzes one `for` range bound, requiring `Int`.
    fn analyze_bound(&mut self, ctx: &mut FnCtx, expr: ExprId) -> HirExprId {
        let span = self.tree.expr(expr).span();
        let hir = self.analyze_expr(ctx, expr);
        let ty = self.program.expr(hir).type_of();
        if ty != Type::Int && ty != Type::Error {
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

    /// Resolves an assignment target into a [`HirPlace`] plus the type stored
    /// there, or `None` when the target does not name a writable place.
    ///
    /// Every step must be writable, not just the last: a struct is a value, so
    /// writing `b.size.x` rewrites the `size` field's contents in place. A
    /// `let` anywhere along the path is what makes that illegal.
    fn resolve_place(&mut self, ctx: &mut FnCtx, target: ExprId) -> Option<(HirPlace, Type)> {
        match self.tree.expr(target).clone() {
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                let Some(local) = ctx.resolve(&name) else {
                    self.emit(
                        span,
                        "KSEM023",
                        format!("cannot assign to undefined name `{name}`"),
                    );
                    return None;
                };
                if !ctx.is_mutable(local) {
                    self.emit(
                        span,
                        "KSEM021",
                        format!(
                            "cannot assign to immutable binding `{name}` (declare it with `var`)"
                        ),
                    );
                    return None;
                }
                Some((
                    HirPlace {
                        local,
                        path: Vec::new(),
                    },
                    ctx.local_type(local),
                ))
            }
            Expr::Field {
                base,
                field,
                field_span,
                ..
            } => {
                let (mut place, base_ty) = self.resolve_place(ctx, base)?;
                let field_name = self.interner.resolve(field).to_owned();
                let (index, field_ty) = self.resolve_field(base_ty, &field_name, field_span)?;
                let mutable = match base_ty {
                    Type::Struct(id) => self
                        .program
                        .structs
                        .get(id)
                        .and_then(|def| def.field(index))
                        .is_some_and(|def| def.mutable),
                    _ => false,
                };
                if !mutable {
                    self.emit(
                        field_span,
                        "KSEM024",
                        format!(
                            "cannot assign to immutable field `{field_name}` of `{}` \
                             (declare it with `var`)",
                            self.type_name(base_ty)
                        ),
                    );
                    return None;
                }
                place.path.push(index);
                Some((place, field_ty))
            }
            other => {
                self.emit(
                    other.span(),
                    "KSEM025",
                    "the left side of an assignment must be a variable or a field of one",
                );
                None
            }
        }
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
