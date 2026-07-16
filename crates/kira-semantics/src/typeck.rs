//! Expression type-checking and operator resolution.
//!
//! Each expression is lowered to a typed [`HirExpr`]. Operators resolve to
//! type-specific HIR variants (e.g. `+` on two `Int`s becomes `AddInt`), so no
//! backend re-derives operand types. Any operand that already analyzed to
//! `Error` short-circuits to another `Error`, suppressing cascades.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirBinaryOp, HirExpr, HirExprId, HirUnaryOp};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, UnaryOp};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Type-checks an AST expression, returning its HIR handle.
    pub(crate) fn analyze_expr(&mut self, ctx: &mut FnCtx, id: ExprId) -> HirExprId {
        let node = self.tree.expr(id).clone();
        match node {
            Expr::Int { value, .. } => self.program.exprs.alloc(HirExpr::Int(value)),
            Expr::Float { value, .. } => self.program.exprs.alloc(HirExpr::Float(value)),
            Expr::Bool { value, .. } => self.program.exprs.alloc(HirExpr::Bool(value)),
            Expr::Str { value, .. } => self.program.exprs.alloc(HirExpr::Str(value)),
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                match ctx.resolve(&name) {
                    Some(local) => {
                        let ty = ctx.local_type(local);
                        self.program.exprs.alloc(HirExpr::Local { local, ty })
                    }
                    None => {
                        self.emit(span, "KSEM060", format!("undefined name `{name}`"));
                        self.program.exprs.alloc(HirExpr::Error)
                    }
                }
            }
            Expr::Unary { op, operand, span } => {
                let operand_hir = self.analyze_expr(ctx, operand);
                let operand_ty = self.program.expr(operand_hir).type_of();
                if operand_ty == Type::Error {
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                match resolve_unary(op, operand_ty) {
                    Some((hir_op, ty)) => self.program.exprs.alloc(HirExpr::Unary {
                        op: hir_op,
                        operand: operand_hir,
                        ty,
                    }),
                    None => {
                        self.emit(
                            span,
                            "KSEM070",
                            format!(
                                "operator `{}` cannot apply to `{}`",
                                unary_spelling(op),
                                operand_ty.name()
                            ),
                        );
                        self.program.exprs.alloc(HirExpr::Error)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, span } => {
                let lhs_hir = self.analyze_expr(ctx, lhs);
                let rhs_hir = self.analyze_expr(ctx, rhs);
                let lt = self.program.expr(lhs_hir).type_of();
                let rt = self.program.expr(rhs_hir).type_of();
                if lt == Type::Error || rt == Type::Error {
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                match resolve_binary(op, lt, rt) {
                    Some((hir_op, ty)) => self.program.exprs.alloc(HirExpr::Binary {
                        op: hir_op,
                        lhs: lhs_hir,
                        rhs: rhs_hir,
                        ty,
                    }),
                    None => {
                        self.emit(
                            span,
                            "KSEM071",
                            format!(
                                "operator `{}` cannot combine `{}` and `{}`",
                                op.spelling(),
                                lt.name(),
                                rt.name()
                            ),
                        );
                        self.program.exprs.alloc(HirExpr::Error)
                    }
                }
            }
            Expr::Call {
                callee,
                callee_span,
                args,
                ..
            } => {
                let name = self.interner.resolve(callee).to_owned();
                let arg_hirs: Vec<HirExprId> = args
                    .iter()
                    .map(|&arg| self.analyze_expr(ctx, arg))
                    .collect();
                if name == "print" {
                    self.analyze_print(&arg_hirs, callee_span)
                } else {
                    self.analyze_user_call(&name, &arg_hirs, callee_span)
                }
            }
            Expr::Error { .. } => self.program.exprs.alloc(HirExpr::Error),
        }
    }

    fn analyze_print(&mut self, args: &[HirExprId], span: kira_source::Span) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`print` takes exactly one argument, found {}", args.len()),
            );
        } else {
            let arg_ty = self.program.expr(args[0]).type_of();
            if arg_ty != Type::Error && !arg_ty.is_printable() {
                self.emit(
                    span,
                    "KSEM081",
                    format!("`print` cannot format a value of type `{}`", arg_ty.name()),
                );
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Print),
            args: args.to_vec(),
            ty: Type::Void,
        })
    }

    fn analyze_user_call(
        &mut self,
        name: &str,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let Some((id, params, ret)) = self
            .lookup_function(name)
            .map(|(id, params, ret)| (id, params.to_vec(), ret))
        else {
            self.emit(
                span,
                "KSEM061",
                format!("call to undefined function `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        if args.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} argument(s), found {}",
                    params.len(),
                    args.len()
                ),
            );
        } else {
            for (index, (&arg, &expected)) in args.iter().zip(params.iter()).enumerate() {
                let actual = self.program.expr(arg).type_of();
                if !actual.assignable_to(expected) {
                    self.emit(
                        span,
                        "KSEM063",
                        format!(
                            "argument {} of `{name}` expects `{}`, found `{}`",
                            index + 1,
                            expected.name(),
                            actual.name()
                        ),
                    );
                }
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(id),
            args: args.to_vec(),
            ty: ret,
        })
    }
}

fn resolve_unary(op: UnaryOp, operand: Type) -> Option<(HirUnaryOp, Type)> {
    match (op, operand) {
        (UnaryOp::Neg, Type::Int) => Some((HirUnaryOp::NegInt, Type::Int)),
        (UnaryOp::Neg, Type::Float) => Some((HirUnaryOp::NegFloat, Type::Float)),
        (UnaryOp::Not, Type::Bool) => Some((HirUnaryOp::Not, Type::Bool)),
        _ => None,
    }
}

fn unary_spelling(op: UnaryOp) -> &'static str {
    match op {
        UnaryOp::Neg => "-",
        UnaryOp::Not => "!",
    }
}

/// Resolves a binary operator against its operand types to a typed HIR op and
/// result type. Returns `None` for an unsupported combination.
fn resolve_binary(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    match op {
        B::Add | B::Sub | B::Mul | B::Div | B::Rem => arithmetic(op, lt, rt),
        B::Lt | B::Le | B::Gt | B::Ge => comparison(op, lt, rt),
        B::Eq | B::Ne => equality(op, lt, rt),
        B::And if lt == Type::Bool && rt == Type::Bool => Some((H::And, Type::Bool)),
        B::Or if lt == Type::Bool && rt == Type::Bool => Some((H::Or, Type::Bool)),
        _ => None,
    }
}

fn arithmetic(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    // String concatenation is the one non-numeric arithmetic case.
    if op == B::Add && lt == Type::String && rt == Type::String {
        return Some((H::ConcatStr, Type::String));
    }
    if lt != rt || !lt.is_numeric() {
        return None;
    }
    let hir = match (op, lt) {
        (B::Add, Type::Int) => H::AddInt,
        (B::Sub, Type::Int) => H::SubInt,
        (B::Mul, Type::Int) => H::MulInt,
        (B::Div, Type::Int) => H::DivInt,
        (B::Rem, Type::Int) => H::RemInt,
        (B::Add, Type::Float) => H::AddFloat,
        (B::Sub, Type::Float) => H::SubFloat,
        (B::Mul, Type::Float) => H::MulFloat,
        (B::Div, Type::Float) => H::DivFloat,
        _ => return None,
    };
    Some((hir, lt))
}

fn comparison(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    if lt != rt || !lt.is_numeric() {
        return None;
    }
    let hir = match (op, lt) {
        (B::Lt, Type::Int) => H::LtInt,
        (B::Le, Type::Int) => H::LeInt,
        (B::Gt, Type::Int) => H::GtInt,
        (B::Ge, Type::Int) => H::GeInt,
        (B::Lt, Type::Float) => H::LtFloat,
        (B::Le, Type::Float) => H::LeFloat,
        (B::Gt, Type::Float) => H::GtFloat,
        (B::Ge, Type::Float) => H::GeFloat,
        _ => return None,
    };
    Some((hir, Type::Bool))
}

fn equality(op: BinaryOp, lt: Type, rt: Type) -> Option<(HirBinaryOp, Type)> {
    use BinaryOp as B;
    use HirBinaryOp as H;
    if lt != rt {
        return None;
    }
    let is_eq = op == B::Eq;
    let hir = match lt {
        Type::Int if is_eq => H::EqInt,
        Type::Int => H::NeInt,
        Type::Float if is_eq => H::EqFloat,
        Type::Float => H::NeFloat,
        Type::Bool if is_eq => H::EqBool,
        Type::Bool => H::NeBool,
        Type::String if is_eq => H::EqStr,
        Type::String => H::NeStr,
        _ => return None,
    };
    Some((hir, Type::Bool))
}
