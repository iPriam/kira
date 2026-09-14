use kira_semantics_model::hir::{
    Callee, FuncId, HirBinaryOp, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirStmt, HirStmtId,
    LocalId,
};
use kira_semantics_model::{OwnershipMode, Type};
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};

#[derive(Clone, Copy)]
struct SortLowering {
    values: LocalId,
    array_ty: Type,
    element: Type,
    compare: LocalId,
    compare_ty: Type,
    dispatcher: FuncId,
}

impl Analyzer<'_> {
    /// Type-checks `xs.sort_by(compare)` as a sorted copy of the array.
    ///
    /// The comparator returns true when its first argument belongs before its
    /// second. Both parameters are borrowed so sorting never consumes an
    /// element merely to compare it.
    pub(super) fn analyze_array_sort_by(
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
        let sort = SortLowering {
            values,
            array_ty,
            element,
            compare,
            compare_ty,
            dispatcher,
        };

        let values_read = self.local_expr(values, array_ty);
        let count = self
            .program
            .exprs
            .alloc(HirExpr::ArrayLen { array: values_read });
        let limit = ctx.declare_hidden(Type::INT, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: limit,
            init: count,
        }));

        let start = ctx.declare_hidden(Type::INT, true);
        let limit_for_start = self.read_int_local(limit);
        let two = self.program.exprs.alloc(HirExpr::Int(2));
        let start_init = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::DivInt,
            lhs: limit_for_start,
            rhs: two,
            ty: Type::INT,
        });
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: start,
            init: start_init,
        }));

        let start_read = self.read_int_local(start);
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        let build_cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::GtInt,
            lhs: start_read,
            rhs: zero,
            ty: Type::Bool,
        });
        let start_before_step = self.read_int_local(start);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let decremented_start = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::SubInt,
            lhs: start_before_step,
            rhs: one,
            ty: Type::INT,
        });
        let step_start = self.assign_local(start, decremented_start);
        let heap_root = self.read_int_local(start);
        let build_sift = self.heap_sift_down(ctx, sort, heap_root, limit);
        let mut build_body = vec![step_start];
        build_body.extend(build_sift);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::While {
            cond: build_cond,
            body: build_body,
        }));

        let end = ctx.declare_hidden(Type::INT, true);
        let limit_for_end = self.read_int_local(limit);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: end,
            init: limit_for_end,
        }));
        let end_read = self.read_int_local(end);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let extract_cond = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::GtInt,
            lhs: end_read,
            rhs: one,
            ty: Type::Bool,
        });
        let end_before_step = self.read_int_local(end);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let decremented_end = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::SubInt,
            lhs: end_before_step,
            rhs: one,
            ty: Type::INT,
        });
        let step_end = self.assign_local(end, decremented_end);
        let root_index = self.program.exprs.alloc(HirExpr::Int(0));
        let end_index = self.read_int_local(end);
        let swap_root = self.swap_indices(ctx, sort, root_index, end_index);
        let root = self.program.exprs.alloc(HirExpr::Int(0));
        let extract_sift = self.heap_sift_down(ctx, sort, root, end);
        let mut extract_body = vec![step_end];
        extract_body.extend(swap_root);
        extract_body.extend(extract_sift);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::While {
            cond: extract_cond,
            body: extract_body,
        }));

        self.local_expr(values, array_ty)
    }

    fn heap_sift_down(
        &mut self,
        ctx: &mut FnCtx,
        sort: SortLowering,
        root_init: HirExprId,
        end: LocalId,
    ) -> Vec<HirStmtId> {
        let root = ctx.declare_hidden(Type::INT, true);
        let bind_root = self.program.stmts.alloc(HirStmt::Let {
            local: root,
            init: root_init,
        });

        let root_for_child = self.read_int_local(root);
        let two = self.program.exprs.alloc(HirExpr::Int(2));
        let doubled = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::MulInt,
            lhs: root_for_child,
            rhs: two,
            ty: Type::INT,
        });
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let first_child = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: doubled,
            rhs: one,
            ty: Type::INT,
        });
        let end_read = self.read_int_local(end);
        let has_child = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: first_child,
            rhs: end_read,
            ty: Type::Bool,
        });

        let candidate = ctx.declare_hidden(Type::INT, true);
        let candidate_init = self.read_int_local(root);
        let bind_candidate = self.program.stmts.alloc(HirStmt::Let {
            local: candidate,
            init: candidate_init,
        });
        let child = ctx.declare_hidden(Type::INT, false);
        let root_for_child = self.read_int_local(root);
        let two = self.program.exprs.alloc(HirExpr::Int(2));
        let doubled = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::MulInt,
            lhs: root_for_child,
            rhs: two,
            ty: Type::INT,
        });
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let child_init = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: doubled,
            rhs: one,
            ty: Type::INT,
        });
        let bind_child = self.program.stmts.alloc(HirStmt::Let {
            local: child,
            init: child_init,
        });

        let candidate_index = self.read_int_local(candidate);
        let child_index = self.read_int_local(child);
        let child_is_larger = self.compare_indices(sort, candidate_index, child_index);
        let child_for_candidate = self.read_int_local(child);
        let choose_child = self.assign_local(candidate, child_for_candidate);
        let choose_child = self.program.stmts.alloc(HirStmt::If {
            cond: child_is_larger,
            then_body: vec![choose_child],
            else_body: Vec::new(),
        });

        let child_read = self.read_int_local(child);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let right = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: child_read,
            rhs: one,
            ty: Type::INT,
        });
        let end_read = self.read_int_local(end);
        let has_right = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::LtInt,
            lhs: right,
            rhs: end_read,
            ty: Type::Bool,
        });
        let candidate_index = self.read_int_local(candidate);
        let child_read = self.read_int_local(child);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let right_index = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: child_read,
            rhs: one,
            ty: Type::INT,
        });
        let right_is_larger = self.compare_indices(sort, candidate_index, right_index);
        let child_read = self.read_int_local(child);
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let right_for_candidate = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: child_read,
            rhs: one,
            ty: Type::INT,
        });
        let choose_right = self.assign_local(candidate, right_for_candidate);
        let choose_right = self.program.stmts.alloc(HirStmt::If {
            cond: right_is_larger,
            then_body: vec![choose_right],
            else_body: Vec::new(),
        });
        let inspect_right = self.program.stmts.alloc(HirStmt::If {
            cond: has_right,
            then_body: vec![choose_right],
            else_body: Vec::new(),
        });

        let candidate_read = self.read_int_local(candidate);
        let root_read = self.read_int_local(root);
        let unchanged = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::EqInt,
            lhs: candidate_read,
            rhs: root_read,
            ty: Type::Bool,
        });
        let leave = self.program.stmts.alloc(HirStmt::Break);
        let root_index = self.read_int_local(root);
        let candidate_index = self.read_int_local(candidate);
        let mut moved = self.swap_indices(ctx, sort, root_index, candidate_index);
        let candidate_read = self.read_int_local(candidate);
        moved.push(self.assign_local(root, candidate_read));
        let advance = self.program.stmts.alloc(HirStmt::If {
            cond: unchanged,
            then_body: vec![leave],
            else_body: moved,
        });

        let sift = self.program.stmts.alloc(HirStmt::While {
            cond: has_child,
            body: vec![bind_candidate, bind_child, choose_child, inspect_right, advance],
        });
        vec![bind_root, sift]
    }

    fn compare_indices(
        &mut self,
        sort: SortLowering,
        left_index: HirExprId,
        right_index: HirExprId,
    ) -> HirExprId {
        let left_base = self.local_expr(sort.values, sort.array_ty);
        let left = self.program.exprs.alloc(HirExpr::Index {
            base: left_base,
            index: left_index,
            ty: sort.element,
        });
        let right_base = self.local_expr(sort.values, sort.array_ty);
        let right = self.program.exprs.alloc(HirExpr::Index {
            base: right_base,
            index: right_index,
            ty: sort.element,
        });
        let compare_read = self.local_expr(sort.compare, sort.compare_ty);
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(sort.dispatcher),
            args: vec![compare_read, left, right],
            ty: Type::Bool,
            writebacks: Vec::new(),
        })
    }

    fn swap_indices(
        &mut self,
        ctx: &mut FnCtx,
        sort: SortLowering,
        left_index: HirExprId,
        right_index: HirExprId,
    ) -> Vec<HirStmtId> {
        let left_base = self.local_expr(sort.values, sort.array_ty);
        let left = self.program.exprs.alloc(HirExpr::Index {
            base: left_base,
            index: left_index,
            ty: sort.element,
        });
        let saved = ctx.declare_hidden(sort.element, false);
        let bind_saved = self.program.stmts.alloc(HirStmt::Let {
            local: saved,
            init: left,
        });
        let right_base = self.local_expr(sort.values, sort.array_ty);
        let right = self.program.exprs.alloc(HirExpr::Index {
            base: right_base,
            index: right_index,
            ty: sort.element,
        });
        let write_left = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: sort.values,
                path: vec![HirPlaceStep::Index(left_index)],
            },
            value: right,
        });
        let saved_read = self.local_expr(saved, sort.element);
        let write_right = self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local: sort.values,
                path: vec![HirPlaceStep::Index(right_index)],
            },
            value: saved_read,
        });
        vec![bind_saved, write_left, write_right]
    }

    fn local_expr(&mut self, local: LocalId, ty: Type) -> HirExprId {
        self.program.exprs.alloc(HirExpr::Local { local, ty })
    }

    fn assign_local(&mut self, local: LocalId, value: HirExprId) -> HirStmtId {
        self.program.stmts.alloc(HirStmt::Assign {
            place: HirPlace {
                local,
                path: Vec::new(),
            },
            value,
        })
    }
}
