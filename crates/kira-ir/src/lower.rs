//! Lowering from the typed HIR to the backend-facing IR.
//!
//! Lowering is a mechanical, total pass: it copies the resolved HIR into the
//! IR's own arena, translating builtin calls to [`IrCallee::Print`] and user
//! calls to slot-indexed [`IrCallee::User`]. It runs only on a program that
//! carries a valid `@Main`; otherwise there is nothing to run and lowering
//! yields `None`.

use kira_semantics_model::hir::{
    Callee, HirExpr, HirExprId, HirPlace, HirPlaceStep, HirProgram, HirStmt, HirStmtId,
};

use crate::ir::{IrCallee, IrExpr, IrExprId, IrFunction, IrPlace, IrPlaceStep, IrProgram, IrStmt};

/// Lowers an analyzed program to IR, or returns `None` when it has no `@Main`.
pub fn lower(program: &HirProgram) -> Option<IrProgram> {
    let main = program.main?;
    let mut ir = IrProgram {
        functions: Vec::with_capacity(program.functions.len()),
        types: program.types.clone(),
        main: main.0,
        exprs: la_arena::Arena::new(),
    };
    let mut lowerer = Lowerer {
        hir: program,
        ir: &mut ir,
    };
    let functions: Vec<IrFunction> = program
        .functions
        .iter()
        .map(|function| lowerer.lower_function(function))
        .collect();
    ir.functions = functions;
    Some(ir)
}

struct Lowerer<'a> {
    hir: &'a HirProgram,
    ir: &'a mut IrProgram,
}

impl Lowerer<'_> {
    fn lower_function(&mut self, function: &kira_semantics_model::hir::HirFunction) -> IrFunction {
        let body = self.lower_stmts(&function.body);
        IrFunction {
            name: function.name.clone(),
            param_count: function.param_count,
            locals: function.locals.iter().map(|local| local.ty).collect(),
            return_type: function.return_type,
            execution: function.execution,
            body,
        }
    }

    fn lower_stmts(&mut self, stmts: &[HirStmtId]) -> Vec<IrStmt> {
        stmts.iter().map(|&id| self.lower_stmt(id)).collect()
    }

    fn lower_stmt(&mut self, id: HirStmtId) -> IrStmt {
        match self.hir.stmt(id).clone() {
            HirStmt::Let { local, init } => IrStmt::Let {
                local: local.0,
                init: self.lower_expr(init),
            },
            HirStmt::Assign { place, value } => IrStmt::Assign {
                place: self.lower_place(&place),
                value: self.lower_expr(value),
            },
            HirStmt::Return { value } => IrStmt::Return {
                value: value.map(|expr| self.lower_expr(expr)),
            },
            HirStmt::Expr { expr } => IrStmt::Eval {
                expr: self.lower_expr(expr),
            },
            HirStmt::If {
                cond,
                then_body,
                else_body,
            } => IrStmt::If {
                cond: self.lower_expr(cond),
                then_body: self.lower_stmts(&then_body),
                else_body: self.lower_stmts(&else_body),
            },
            HirStmt::While { cond, body } => IrStmt::While {
                cond: self.lower_expr(cond),
                body: self.lower_stmts(&body),
            },
            HirStmt::Break => IrStmt::Break,
            HirStmt::Continue => IrStmt::Continue,
        }
    }

    fn lower_expr(&mut self, id: HirExprId) -> IrExprId {
        let node = match self.hir.expr(id).clone() {
            HirExpr::Int(value) => IrExpr::Int(value),
            HirExpr::Float(value) => IrExpr::Float(value),
            HirExpr::Bool(value) => IrExpr::Bool(value),
            HirExpr::Str(value) => IrExpr::Str(value),
            HirExpr::Local { local, .. } => IrExpr::Local(local.0),
            HirExpr::Unary { op, operand, .. } => IrExpr::Unary {
                op,
                operand: self.lower_expr(operand),
            },
            HirExpr::Binary { op, lhs, rhs, .. } => IrExpr::Binary {
                op,
                lhs: self.lower_expr(lhs),
                rhs: self.lower_expr(rhs),
            },
            HirExpr::Select {
                cond,
                then,
                otherwise,
                ty,
            } => IrExpr::Select {
                cond: self.lower_expr(cond),
                then: self.lower_expr(then),
                otherwise: self.lower_expr(otherwise),
                ty,
            },
            HirExpr::Call { callee, args, ty } => {
                let ir_args = args.iter().map(|&arg| self.lower_expr(arg)).collect();
                IrExpr::Call {
                    callee: lower_callee(callee),
                    args: ir_args,
                    result: ty,
                }
            }
            HirExpr::StructNew { struct_id, fields } => {
                let ir_fields = fields.iter().map(|&field| self.lower_expr(field)).collect();
                IrExpr::StructNew {
                    struct_id,
                    fields: ir_fields,
                }
            }
            HirExpr::Field { base, index, ty } => IrExpr::Field {
                base: self.lower_expr(base),
                index,
                ty,
            },
            HirExpr::ArrayNew { ty, elements } => {
                let ir_elements = elements
                    .iter()
                    .map(|&element| self.lower_expr(element))
                    .collect();
                IrExpr::ArrayNew {
                    ty,
                    elements: ir_elements,
                }
            }
            HirExpr::Index { base, index, ty } => IrExpr::Index {
                base: self.lower_expr(base),
                index: self.lower_expr(index),
                ty,
            },
            HirExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => IrExpr::EnumNew {
                enum_id,
                tag,
                payload: payload.map(|expr| self.lower_expr(expr)),
            },
            HirExpr::EnumTag { value } => IrExpr::EnumTag {
                value: self.lower_expr(value),
            },
            HirExpr::EnumPayload { value, ty } => IrExpr::EnumPayload {
                value: self.lower_expr(value),
                ty,
            },
            HirExpr::ArrayLen { array } => IrExpr::ArrayLen {
                array: self.lower_expr(array),
            },
            HirExpr::ArrayAppend { place, value } => IrExpr::ArrayAppend {
                place: self.lower_place(&place),
                value: self.lower_expr(value),
            },
            // An error node can only be reached when analysis already reported
            // diagnostics and the program is never run; lower it to a harmless
            // constant so lowering stays total.
            HirExpr::Error => IrExpr::Int(0),
        };
        self.ir.exprs.alloc(node)
    }

    /// Lowers a place, lowering the index expressions its path carries.
    ///
    /// A place is not a plain data copy any more: an `Index` step holds an
    /// expression, which has to land in the IR's own arena like every other.
    fn lower_place(&mut self, place: &HirPlace) -> IrPlace {
        let path = place
            .path
            .iter()
            .map(|step| match step {
                HirPlaceStep::Field(index) => IrPlaceStep::Field(*index),
                HirPlaceStep::Index(expr) => IrPlaceStep::Index(self.lower_expr(*expr)),
            })
            .collect();
        IrPlace {
            local: place.local.0,
            path,
        }
    }
}

fn lower_callee(callee: Callee) -> IrCallee {
    match callee {
        Callee::Builtin(kira_semantics_model::hir::Builtin::Print) => IrCallee::Print,
        Callee::User(id) => IrCallee::User(id.0),
    }
}
