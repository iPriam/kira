//! The two `for`-loop desugars: `for i in a..b` and `for x in xs`, each
//! rewritten into the `while` it already means.
//!
//! Neither adds an IR node, an opcode, or any backend work — the same trade the
//! range form and `switch` already made. The correctness of both turns on one
//! detail: the cursor is stepped *before* the body, so a `continue` cannot jump
//! over the increment and spin forever.

use kira_core::Symbol;
use kira_semantics_model::hir::{HirExprId, HirPlace, HirStmt, HirStmtId};
use kira_semantics_model::{HirBinaryOp, HirExpr, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};

/// The loop variable as written: its name and the name's span.
///
/// One value because the two travel together — the span is where a
/// go-to-definition jump on a use of the variable lands.
#[derive(Clone, Copy)]
pub(crate) struct ForCursor {
    pub(crate) name: Symbol,
    pub(crate) span: Span,
}

#[derive(Clone, Copy)]
struct IteratorLoop {
    cursor: ForCursor,
    expr: HirExprId,
    ty: Type,
    element: Type,
    span: Span,
}

impl Analyzer<'_> {
    /// The element of Foundation's lazy `Iterator<Value>` type.
    fn iterator_element(&self, ty: Type) -> Option<Type> {
        let Type::Struct(id) = ty else {
            return None;
        };
        let instantiation = self.program.types.structs().instantiation(id)?;
        (instantiation.template == "Foundation::Iterator" && instantiation.arguments.len() == 1)
            .then_some(instantiation.arguments[0])
    }

    /// Desugars a lazy `Iterator<Value>` into a pull loop.
    ///
    /// The iterator is evaluated once. Each iteration calls its `next` closure,
    /// runs the user's body for `.Some(value)`, and breaks on `.None`. Because
    /// the pull happens before the user body, `continue` naturally advances to
    /// the next value instead of repeating the current one.
    fn analyze_for_iterator(
        &mut self,
        ctx: &mut FnCtx,
        iteration: IteratorLoop,
        out: &mut Vec<HirStmtId>,
        fill_body: impl FnOnce(&mut Self, &mut FnCtx, &mut Vec<HirStmtId>),
    ) {
        let IteratorLoop {
            cursor,
            expr,
            ty,
            element,
            span,
        } = iteration;
        if self.program.types.runs_user_drop(element) {
            self.refuse_drop_extraction(element, cursor.span);
        }

        let Type::Struct(iterator_id) = ty else {
            return;
        };
        let Some(definition) = self.program.types.structs().get(iterator_id) else {
            return;
        };
        let Some(next_index) = definition.field_index("next") else {
            return;
        };
        let Some(next_ty) = definition.field(next_index).map(|field| field.ty) else {
            return;
        };
        let Some(next_repr) = self.as_function_type(next_ty) else {
            return;
        };

        let iterator_slot = ctx.declare_hidden(ty, false);
        out.push(self.program.stmts.alloc(HirStmt::Let {
            local: iterator_slot,
            init: expr,
        }));

        // Build the `next()` call before declaring its storage so its result
        // type is the source of truth for the Option specialization.
        let iterator_read = self.program.exprs.alloc(HirExpr::Local {
            local: iterator_slot,
            ty,
        });
        let next_value = self.program.exprs.alloc(HirExpr::Field {
            base: iterator_read,
            index: next_index,
            ty: next_ty,
        });
        let next_call = self.analyze_closure_call(ctx, next_value, next_repr, &[], span);
        let option_ty = self.program.expr(next_call).type_of();
        let Type::Enum(option_id) = option_ty else {
            return;
        };
        let Some(option) = self.program.types.enums().get(option_id) else {
            return;
        };
        let Some(some_tag) = option.variant_index("Some") else {
            return;
        };
        let Some(payload_ty) = option.variant(some_tag).and_then(|variant| variant.payload) else {
            return;
        };
        if payload_ty != element && payload_ty != Type::Error && element != Type::Error {
            return;
        }

        let next_slot = ctx.declare_hidden(option_ty, false);
        let bind_next = self.program.stmts.alloc(HirStmt::Let {
            local: next_slot,
            init: next_call,
        });
        let option_read = self.program.exprs.alloc(HirExpr::Local {
            local: next_slot,
            ty: option_ty,
        });
        let tag = self
            .program
            .exprs
            .alloc(HirExpr::EnumTag { value: option_read });
        let wanted = self.program.exprs.alloc(HirExpr::Int(i64::from(some_tag)));
        let has_value = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::EqInt,
            lhs: tag,
            rhs: wanted,
            ty: Type::Bool,
        });

        let loop_moves = crate::ownership::LoopMoves::start(ctx);
        ctx.push_scope();
        let user_name = self.interner.resolve(cursor.name).to_owned();
        let variable = ctx.declare(&user_name, element, false);
        ctx.note_binding_span(variable, cursor.span);
        let payload_base = self.program.exprs.alloc(HirExpr::Local {
            local: next_slot,
            ty: option_ty,
        });
        let payload = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: payload_base,
            ty: element,
        });
        let bind_value = self.program.stmts.alloc(HirStmt::Let {
            local: variable,
            init: payload,
        });
        let mut some_body = vec![bind_value];
        ctx.loop_depth += 1;
        ctx.push_scope();
        fill_body(self, ctx, &mut some_body);
        ctx.pop_scope();
        ctx.loop_depth -= 1;
        ctx.pop_scope();

        let stop = self.program.stmts.alloc(HirStmt::Break);
        let branch = self.program.stmts.alloc(HirStmt::If {
            cond: has_value,
            then_body: some_body,
            else_body: vec![stop],
        });
        let loop_body = vec![bind_next, branch];
        let exits = self.body_always_exits_loop(&loop_body);
        self.check_loop_back_edge(ctx, loop_moves, exits);
        let always = self.program.exprs.alloc(HirExpr::Bool(true));
        out.push(self.program.stmts.alloc(HirStmt::While {
            cond: always,
            body: loop_body,
        }));
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
    pub(super) fn analyze_for_range(
        &mut self,
        ctx: &mut FnCtx,
        cursor_name: ForCursor,
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

        // Taken before the loop variable exists, so the variable — rebound at
        // the top of every iteration — is not one of the locals the back-edge
        // check can blame.
        let loop_moves = crate::ownership::LoopMoves::start(ctx);

        // The user's variable lives in its own scope: it is visible to the
        // body and gone afterwards.
        ctx.push_scope();
        let user_name = self.interner.resolve(cursor_name.name).to_owned();
        let variable = ctx.declare(&user_name, Type::INT, false);
        ctx.note_binding_span(variable, cursor_name.span);
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
        let exits = self.body_always_exits_loop(&loop_body);
        self.check_loop_back_edge(ctx, loop_moves, exits);

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
    /// Desugars a `for <name> in <xs>` loop, filling its body via `fill_body`.
    ///
    /// The statement form fills the body by analyzing the written block; a
    /// builder content item fills it by appending each produced child to the
    /// slot accumulator. Both run inside the loop's scope with the cursor
    /// variable bound, so the desugar — and the parity it already has — is
    /// shared rather than duplicated.
    pub(crate) fn analyze_for_each(
        &mut self,
        ctx: &mut FnCtx,
        cursor_name: ForCursor,
        array: ExprId,
        span: Span,
        out: &mut Vec<HirStmtId>,
        fill_body: impl FnOnce(&mut Self, &mut FnCtx, &mut Vec<HirStmtId>),
    ) {
        // `for x in []` iterates nothing, so the element type it would need is
        // a type no code can observe: the body never runs and `x` is never
        // bound. Recognized from the *syntax*, before the literal is analyzed
        // and asked what it holds, because that question is the one `KSEM104`
        // refuses to guess at — and here there is nothing to guess for.
        if matches!(self.tree.expr(array), Expr::ArrayLit { elements, .. } if elements.is_empty()) {
            ctx.push_scope();
            let mut discarded = Vec::new();
            fill_body(self, ctx, &mut discarded);
            ctx.pop_scope();
            return;
        }
        let (array, reverse) = match self.tree.expr(array) {
            Expr::MethodCall {
                receiver,
                method,
                args,
                children,
                ..
            } if args.is_empty()
                && children.is_empty()
                && self.interner.resolve(*method) == "rev" =>
            {
                (*receiver, true)
            }
            _ => (array, false),
        };
        let array_span = self.tree.expr(array).span();
        let array_expr = self.analyze_expr(ctx, array);
        let array_ty = self.program.expr(array_expr).type_of();

        if let Some(element) = self.iterator_element(array_ty) {
            self.analyze_for_iterator(
                ctx,
                IteratorLoop {
                    cursor: cursor_name,
                    expr: array_expr,
                    ty: array_ty,
                    element,
                    span,
                },
                out,
                fill_body,
            );
            return;
        }

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
                             (`for x in xs`), an `Iterator<T>`, or a range (`for i in 0..n`)",
                            self.type_name(array_ty)
                        ),
                    );
                }
                Type::Error
            }
        };

        // The cursor binds one element per iteration while the array still
        // holds it, which for a value running a user `Drop` is the second owner
        // a member read is refused for. Refused here rather than at the
        // synthesized index read: this desugar builds the read itself, so there
        // is no enclosing expression to claim it as a borrowed one.
        if self.program.types.runs_user_drop(element) {
            self.refuse_drop_extraction(element, cursor_name.span);
        }

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
        let cursor_init = if reverse {
            self.read_int_local(limit)
        } else {
            self.program.exprs.alloc(HirExpr::Int(0))
        };
        let bind_cursor = self.program.stmts.alloc(HirStmt::Let {
            local: cursor,
            init: cursor_init,
        });
        out.push(bind_cursor);

        let cursor_read = self.read_int_local(cursor);
        let cond = if reverse {
            let zero = self.program.exprs.alloc(HirExpr::Int(0));
            self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::GtInt,
                lhs: cursor_read,
                rhs: zero,
                ty: Type::Bool,
            })
        } else {
            let limit_read = self.read_int_local(limit);
            self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::LtInt,
                lhs: cursor_read,
                rhs: limit_read,
                ty: Type::Bool,
            })
        };

        // Taken before the loop variable exists, for the reason the range form
        // states: the variable is rebound every iteration, so its move never
        // crosses the back edge.
        let loop_moves = crate::ownership::LoopMoves::start(ctx);

        // The user's variable lives in its own scope: visible to the body,
        // gone afterwards.
        ctx.push_scope();
        let user_name = self.interner.resolve(cursor_name.name).to_owned();
        let variable = ctx.declare(&user_name, element, false);
        ctx.note_binding_span(variable, cursor_name.span);
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: array_slot,
            ty: array_ty,
        });
        let index = if reverse {
            let cursor_read = self.read_int_local(cursor);
            let one = self.program.exprs.alloc(HirExpr::Int(1));
            self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::SubInt,
                lhs: cursor_read,
                rhs: one,
                ty: Type::INT,
            })
        } else {
            self.read_int_local(cursor)
        };
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
            op: if reverse {
                HirBinaryOp::SubInt
            } else {
                HirBinaryOp::AddInt
            },
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
        fill_body(self, ctx, &mut loop_body);
        ctx.pop_scope();
        ctx.loop_depth -= 1;
        ctx.pop_scope();
        let exits = self.body_always_exits_loop(&loop_body);
        self.check_loop_back_edge(ctx, loop_moves, exits);

        let _ = span;
        let hir = self.program.stmts.alloc(HirStmt::While {
            cond,
            body: loop_body,
        });
        out.push(hir);
    }
}
