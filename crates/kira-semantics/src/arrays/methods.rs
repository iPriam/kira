use kira_semantics_model::hir::{
    Callee, HirBinaryOp, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirStmt,
};
use kira_semantics_model::{OwnershipMode, Type};
use kira_source::Span;
use kira_syntax_model::ast::{BinaryOp, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::resolve_binary;
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
        let Some((eq, _)) = resolve_binary(BinaryOp::Eq, element, needle_ty) else {
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

    /// Type-checks `xs.sort_by(compare)` as a sorted copy of the array.
    ///
    /// The comparator returns true when its first argument belongs before its
    /// second. Both parameters are borrowed so sorting never consumes an
    /// element merely to compare it.
    fn analyze_array_sort_by(
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
                format!("`sort_by` takes 1 comparator, found {}", args.len()),
            );
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if self.program.types.runs_user_drop(element) {
            self.refuse_drop_extraction(element, method_span);
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let compare_ty = self.function_type(
            vec![element, element],
            vec![OwnershipMode::BorrowRead, OwnershipMode::BorrowRead],
            Type::Bool,
        );
        let comparator = self.analyze_expr_expecting(ctx, args[0], Some(compare_ty));
        if self.program.expr(comparator).type_of() != compare_ty {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Some(compare_repr) = self.as_function_type(compare_ty) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let dispatcher = self.dispatcher_for(compare_repr);
        self.excuse_drop_extraction(array);

        let values = ctx.declare_hidden(array_ty, true);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: values,
            init: array,
        }));
        let compare = ctx.declare_hidden(compare_ty, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: compare,
            init: comparator,
        }));

        let values_read = self.program.exprs.alloc(HirExpr::Local {
            local: values,
            ty: array_ty,
        });
        let count = self
            .program
            .exprs
            .alloc(HirExpr::ArrayLen { array: values_read });
        let limit = ctx.declare_hidden(Type::INT, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: limit,
            init: count,
        }));
        let i = ctx.declare_hidden(Type::INT, true);
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: i,
            init: zero,
        }));

        let i_read = self.read_int_local(i);
        let limit_read = self.read_int_local(limit);
        let outer_cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: i_read,
            rhs: limit_read,
            ty: Type::Bool,
        });

        let best = ctx.declare_hidden(Type::INT, true);
        let best_init = self.read_int_local(i);
        let bind_best = self.program.stmts.alloc(HirStmt::Let {
            local: best,
            init: best_init,
        });
        let j = ctx.declare_hidden(Type::INT, true);
        let i_for_j = self.read_int_local(i);
        let one_for_j = self.program.exprs.alloc(HirExpr::Int(1));
        let j_init = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: i_for_j,
            rhs: one_for_j,
            ty: Type::INT,
        });
        let bind_j = self.program.stmts.alloc(HirStmt::Let {
            local: j,
            init: j_init,
        });

        let j_read = self.read_int_local(j);
        let limit_inner = self.read_int_local(limit);
        let inner_cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: j_read,
            rhs: limit_inner,
            ty: Type::Bool,
        });
        let left_base = self.program.exprs.alloc(HirExpr::Local {
            local: values,
            ty: array_ty,
        });
        let left_index = self.read_int_local(j);
        let left = self.program.exprs.alloc(HirExpr::Index {
            base: left_base,
            index: left_index,
            ty: element,
        });
        let right_base = self.program.exprs.alloc(HirExpr::Local {
            local: values,
            ty: array_ty,
        });
        let right_index = self.read_int_local(best);
        let right = self.program.exprs.alloc(HirExpr::Index {
            base: right_base,
            index: right_index,
            ty: element,
        });
        let compare_read = self.program.exprs.alloc(HirExpr::Local {
            local: compare,
            ty: compare_ty,
        });
        let comes_before = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: vec![compare_read, left, right],
            ty: Type::Bool,
            writebacks: Vec::new(),
        });
        let j_for_best = self.read_int_local(j);
        let choose = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: best,
                path: Vec::new(),
            },
            value: j_for_best,
        });
        let choose = self.program.stmts.alloc(HirStmt::If {
            cond: comes_before,
            then_body: vec![choose],
            else_body: Vec::new(),
        });
        let j_step_read = self.read_int_local(j);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let j_stepped = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: j_step_read,
            rhs: one,
            ty: Type::INT,
        });
        let j_step = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: j,
                path: Vec::new(),
            },
            value: j_stepped,
        });
        let inner = self.program.stmts.alloc(HirStmt::While {
            cond: inner_cond,
            body: vec![choose, j_step],
        });

        let best_read = self.read_int_local(best);
        let i_for_compare = self.read_int_local(i);
        let different = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::NeInt,
            lhs: best_read,
            rhs: i_for_compare,
            ty: Type::Bool,
        });
        let i_base = self.program.exprs.alloc(HirExpr::Local {
            local: values,
            ty: array_ty,
        });
        let i_index = self.read_int_local(i);
        let current = self.program.exprs.alloc(HirExpr::Index {
            base: i_base,
            index: i_index,
            ty: element,
        });
        let saved = ctx.declare_hidden(element, false);
        let bind_saved = self.program.stmts.alloc(HirStmt::Let {
            local: saved,
            init: current,
        });
        let best_base = self.program.exprs.alloc(HirExpr::Local {
            local: values,
            ty: array_ty,
        });
        let best_index = self.read_int_local(best);
        let selected = self.program.exprs.alloc(HirExpr::Index {
            base: best_base,
            index: best_index,
            ty: element,
        });
        let i_place_index = self.read_int_local(i);
        let write_i = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: values,
                path: vec![HirPlaceStep::Index(i_place_index)],
            },
            value: selected,
        });
        let saved_read = self.program.exprs.alloc(HirExpr::Local {
            local: saved,
            ty: element,
        });
        let best_place_index = self.read_int_local(best);
        let write_best = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: values,
                path: vec![HirPlaceStep::Index(best_place_index)],
            },
            value: saved_read,
        });
        let swap = self.program.stmts.alloc(HirStmt::If {
            cond: different,
            then_body: vec![bind_saved, write_i, write_best],
            else_body: Vec::new(),
        });

        let i_step_read = self.read_int_local(i);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let i_stepped = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: i_step_read,
            rhs: one,
            ty: Type::INT,
        });
        let i_step = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: i,
                path: Vec::new(),
            },
            value: i_stepped,
        });
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::While {
            cond: outer_cond,
            body: vec![bind_best, bind_j, inner, swap, i_step],
        }));
        self.program.exprs.alloc(HirExpr::Local {
            local: values,
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
