//! The two `for`-loop desugars: `for i in a..b` and `for x in xs`, each
//! rewritten into the `while` it already means.
//!
//! Neither adds an IR node, an opcode, or any backend work — the same trade the
//! range form and `switch` already made. The correctness of both turns on one
//! detail: the cursor is stepped *before* the body, so a `continue` cannot jump
//! over the increment and spin forever.

use kira_core::Symbol;
use kira_semantics_model::hir::{HirPlace, HirStmt, HirStmtId};
use kira_semantics_model::{HirBinaryOp, HirExpr, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ExprId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
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
    pub(super) fn analyze_for_range(
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
        let cursor = ctx.declare_hidden(Type::INT, true);
        let limit = ctx.declare_hidden(Type::INT, false);
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
        let variable = ctx.declare(&user_name, Type::INT, false);
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
            ty: Type::INT,
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

    /// Desugars `for <name> in <xs> { … }` into a `while`.
    ///
    /// The rewrite, given `for x in xs { body }`:
    ///
    /// ```text
    /// let  <array> = xs              // hidden: evaluated once
    /// let  <limit> = <array>.count   // hidden: measured once
    /// var  <cursor> = 0              // hidden: the iteration state
    /// while <cursor> < <limit> {
    ///     let x = <array>[<cursor>]  // the user's variable: immutable, a copy
    ///     <cursor> = <cursor> + 1
    ///     body
    /// }
    /// ```
    ///
    /// This costs **zero** new IR nodes, opcodes, and backend work: it is
    /// `while`, `<`, `+`, an index read, and `.count` — all of which the array
    /// feature already had to carry. The same trade the `for`-over-range and
    /// `switch` desugars already made.
    ///
    /// Four details carry the correctness argument:
    ///
    /// * **The increment precedes the body**, for exactly the reason it does in
    ///   the range form: putting it last would let `continue` jump over it and
    ///   spin forever.
    /// * **`x` is a fresh immutable copy**, so a body cannot perturb the
    ///   iteration by writing to it — it is a `let`.
    /// * **The array is bound to a hidden local**, so `for x in makeRows()`
    ///   builds the array once rather than once per test.
    /// * **The count is measured once**, into its own hidden local. Re-reading
    ///   `<array>.count` on every test would be a fresh copy of the whole array
    ///   per iteration.
    ///
    /// The hidden locals are bound to **no name**, so a body cannot read,
    /// write, or shadow them — not even one that declares a variable spelled
    /// the same way — because name resolution only consults the scope stack.
    ///
    /// The hidden `let <array> = xs` does **not** consume `xs`: implicit
    /// move-on-bind applies to a binding the *user wrote*
    /// ([`Analyzer::apply_binding_move`] is called from the `let` arm), and
    /// this statement is built here. So `for x in xs { }` leaves `xs` usable
    /// afterwards, which is what a reader expects of a loop that only reads.
    pub(super) fn analyze_for_each(
        &mut self,
        ctx: &mut FnCtx,
        name: Symbol,
        array: ExprId,
        body: &Block,
        span: Span,
        out: &mut Vec<HirStmtId>,
    ) {
        let array_span = self.tree.expr(array).span();
        let array_expr = self.analyze_expr(ctx, array);
        let array_ty = self.program.expr(array_expr).type_of();

        // What the loop variable holds. An `Error` element keeps the loop
        // building — the body still analyzes, and its own mistakes are still
        // reported — rather than swallowing the block.
        let element = match self.program.types.element_of(array_ty) {
            Some(element) => element,
            None => {
                if array_ty != Type::Error {
                    self.emit(
                        array_span,
                        "KSEM106",
                        format!(
                            "cannot iterate a value of type `{}`; `for` takes an array \
                             (`for x in xs`) or a range (`for i in 0..n`)",
                            self.type_name(array_ty)
                        ),
                    );
                }
                Type::Error
            }
        };

        let array_slot = ctx.declare_hidden(array_ty, false);
        let bind_array = self.program.stmts.alloc(HirStmt::Let {
            local: array_slot,
            init: array_expr,
        });
        out.push(bind_array);

        // `<limit> = <array>.count`, measured once.
        let array_read = self.program.exprs.alloc(HirExpr::Local {
            local: array_slot,
            ty: array_ty,
        });
        let count = self
            .program
            .exprs
            .alloc(HirExpr::ArrayLen { array: array_read });
        let limit = ctx.declare_hidden(Type::INT, false);
        let bind_limit = self.program.stmts.alloc(HirStmt::Let {
            local: limit,
            init: count,
        });
        out.push(bind_limit);

        let cursor = ctx.declare_hidden(Type::INT, true);
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        let bind_cursor = self.program.stmts.alloc(HirStmt::Let {
            local: cursor,
            init: zero,
        });
        out.push(bind_cursor);

        let cursor_read = self.read_int_local(cursor);
        let limit_read = self.read_int_local(limit);
        let cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: cursor_read,
            rhs: limit_read,
            ty: Type::Bool,
        });

        // The user's variable lives in its own scope: visible to the body,
        // gone afterwards.
        ctx.push_scope();
        let user_name = self.interner.resolve(name).to_owned();
        let variable = ctx.declare(&user_name, element, false);
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: array_slot,
            ty: array_ty,
        });
        let index = self.read_int_local(cursor);
        let read = self.program.exprs.alloc(HirExpr::Index {
            base,
            index,
            ty: element,
        });
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: variable,
            init: read,
        });

        let step_read = self.read_int_local(cursor);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let stepped = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: step_read,
            rhs: one,
            ty: Type::INT,
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

        let _ = span;
        let hir = self.program.stmts.alloc(HirStmt::While {
            cond,
            body: loop_body,
        });
        out.push(hir);
    }
}
