mod sort;

use kira_semantics_model::hir::{HirBinaryOp, HirExpr, HirExprId, HirPlace, HirStmt};
use kira_semantics_model::Type;
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

use super::unsupported_member;

impl Analyzer<'_> {
    /// Type-checks one array method call and routes it to the matching lowering.
    pub(crate) fn analyze_array_method(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        name: &str,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        if name == "count" {
            self.emit(
                method_span,
                "KSEM101",
                "`count` is a property: write `xs.count`, without parentheses",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        match name {
            "append" => self.analyze_array_append(ctx, receiver, method_span, args),
            "contains" => self.analyze_array_contains(ctx, receiver, method_span, args),
            "rev" => self.analyze_array_rev(ctx, receiver, method_span, args),
            "sort_by" => self.analyze_array_sort_by(ctx, receiver, method_span, args),
            _ => {
                self.emit(method_span, "KSEM101", unsupported_member(name));
                self.program.exprs.alloc(HirExpr::Error)
            }
        }
    }

    /// Type-checks `xs.contains(v)` by lowering it to one linear scan.
    fn analyze_array_contains(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        let array = self.analyze_expr(ctx, receiver);
        let array_ty = self.program.expr(array).type_of();
        let Some(element) = self.program.types.element_of(array_ty) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        if args.len() != 1 {
            self.emit(
                method_span,
                "KSEM103",
                format!("`contains` takes 1 argument, found {}", args.len()),
            );
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let needle = self.analyze_expr_expecting(ctx, args[0], Some(element));
        let needle_ty = self.program.expr(needle).type_of();
        // The needle is checked against the element type above, so a mismatch is
        // already an error; the remaining question is whether that element type
        // has equality at all — the same structural rule `==` uses, so an array
        // of structs or payload-carrying enums is searchable exactly when those
        // values are comparable.
        let eq = if element != needle_ty {
            None
        } else {
            self.equality_op(element)
        };
        let Some(eq) = eq else {
            self.emit(
                self.tree.expr(args[0]).span(),
                "KSEM389",
                format!(
                    "`contains` needs elements that can be compared with `==`; `{}` and `{}` cannot be compared",
                    self.type_name(element),
                    self.type_name(needle_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };

        self.excuse_drop_extraction(array);
        let array_slot = ctx.declare_hidden(array_ty, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: array_slot,
            init: array,
        }));
        let needle_slot = ctx.declare_hidden(element, false);
        let needle = self.coerce_into(needle, element);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: needle_slot,
            init: needle,
        }));
        let found = ctx.declare_hidden(Type::Bool, true);
        let false_value = self.program.exprs.alloc(HirExpr::Bool(false));
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: found,
            init: false_value,
        }));
        let index = ctx.declare_hidden(Type::INT, true);
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: index,
            init: zero,
        }));

        let array_read = self.program.exprs.alloc(HirExpr::Local {
            local: array_slot,
            ty: array_ty,
        });
        let count = self
            .program
            .exprs
            .alloc(HirExpr::ArrayLen { array: array_read });
        let index_read = self.read_int_local(index);
        let cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: index_read,
            rhs: count,
            ty: Type::Bool,
        });

        let base = self.program.exprs.alloc(HirExpr::Local {
            local: array_slot,
            ty: array_ty,
        });
        let position = self.read_int_local(index);
        let current = self.program.exprs.alloc(HirExpr::Index {
            base,
            index: position,
            ty: element,
        });
        let wanted = self.program.exprs.alloc(HirExpr::Local {
            local: needle_slot,
            ty: element,
        });
        let equal = self.program.exprs.alloc(HirExpr::Binary {
            op: eq,
            lhs: current,
            rhs: wanted,
            ty: Type::Bool,
        });
        let true_value = self.program.exprs.alloc(HirExpr::Bool(true));
        let mark_found = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: found,
                path: Vec::new(),
            },
            value: true_value,
        });
        let leave = self.program.stmts.alloc(HirStmt::Break);
        let hit = self.program.stmts.alloc(HirStmt::If {
            cond: equal,
            then_body: vec![mark_found, leave],
            else_body: Vec::new(),
        });
        let step_read = self.read_int_local(index);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let stepped = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: step_read,
            rhs: one,
            ty: Type::INT,
        });
        let step = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: index,
                path: Vec::new(),
            },
            value: stepped,
        });
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::While {
            cond,
            body: vec![hit, step],
        }));
        self.program.exprs.alloc(HirExpr::Local {
            local: found,
            ty: Type::Bool,
        })
    }

    /// Type-checks `xs.rev()` as a reversed array value.
    fn analyze_array_rev(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        if !args.is_empty() {
            self.emit(
                method_span,
                "KSEM103",
                format!("`rev` takes no arguments, found {}", args.len()),
            );
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let array = self.analyze_expr(ctx, receiver);
        let array_ty = self.program.expr(array).type_of();
        let Some(element) = self.program.types.element_of(array_ty) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        if self.program.types.runs_user_drop(element) {
            self.refuse_drop_extraction(element, method_span);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.excuse_drop_extraction(array);

        let source = ctx.declare_hidden(array_ty, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: source,
            init: array,
        }));
        let result = ctx.declare_hidden(array_ty, true);
        let empty = self.program.exprs.alloc(HirExpr::ArrayNew {
            ty: array_ty,
            elements: Vec::new(),
        });
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: result,
            init: empty,
        }));
        let source_read = self.program.exprs.alloc(HirExpr::Local {
            local: source,
            ty: array_ty,
        });
        let count = self
            .program
            .exprs
            .alloc(HirExpr::ArrayLen { array: source_read });
        let index = ctx.declare_hidden(Type::INT, true);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: index,
            init: count,
        }));

        let index_read = self.read_int_local(index);
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        let cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::GtInt,
            lhs: index_read,
            rhs: zero,
            ty: Type::Bool,
        });
        let prior_read = self.read_int_local(index);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let prior = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::SubInt,
            lhs: prior_read,
            rhs: one,
            ty: Type::INT,
        });
        let step = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: index,
                path: Vec::new(),
            },
            value: prior,
        });
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: source,
            ty: array_ty,
        });
        let position = self.read_int_local(index);
        let value = self.program.exprs.alloc(HirExpr::Index {
            base,
            index: position,
            ty: element,
        });
        let append = self.program.exprs.alloc(HirExpr::ArrayAppend {
            place: HirPlace {
                local: result,
                path: Vec::new(),
            },
            value,
        });
        let append = self.program.stmts.alloc(HirStmt::Expr { expr: append });
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::While {
            cond,
            body: vec![step, append],
        }));
        self.program.exprs.alloc(HirExpr::Local {
            local: result,
            ty: array_ty,
        })
    }


    /// Type-checks `xs.append(v)`.
    fn analyze_array_append(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        // The receiver is resolved to a place *first*, so `append` on something
        // that is not a place is refused before its argument is analyzed
        // against an element type there is no array to supply.
        let Some((place, place_ty)) = self.resolve_place(ctx, receiver, PlacePurpose::Append)
        else {
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let element = self.program.types.element_of(place_ty);

        if args.len() != 1 {
            self.emit(
                method_span,
                "KSEM103",
                format!("`append` takes exactly one argument, found {}", args.len()),
            );
            for &arg in args {
                self.analyze_expr_expecting(ctx, arg, element);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let value = self.analyze_expr_expecting(ctx, args[0], element);
        let Some(element) = element else {
            // The place resolved but is not an array. `resolve_place` reports
            // the shape problems; this reports the type one.
            if place_ty != Type::Error {
                self.emit(
                    method_span,
                    "KSEM101",
                    format!("type `{}` has no method `append`", self.type_name(place_ty)),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let value_ty = self.program.expr(value).type_of();
        if !self.admits(value_ty, element) {
            let span = self.tree.expr(args[0]).span();
            self.emit(
                span,
                "KSEM105",
                format!(
                    "cannot append a `{}` to an array of `{}`",
                    self.type_name(value_ty),
                    self.type_name(element)
                ),
            );
        }
        let value = self.coerce_into(value, element);
        self.program
            .exprs
            .alloc(HirExpr::ArrayAppend { place, value })
    }
}
