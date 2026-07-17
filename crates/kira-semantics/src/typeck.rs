//! Expression type-checking and operator resolution.
//!
//! Each expression is lowered to a typed [`HirExpr`]. Operators resolve to
//! type-specific HIR variants (e.g. `+` on two `Int`s becomes `AddInt`), so no
//! backend re-derives operand types. Any operand that already analyzed to
//! `Error` short-circuits to another `Error`, suppressing cascades.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirBinaryOp, HirExpr, HirExprId, HirUnaryOp};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, FieldInit, UnaryOp};

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
                    // A local wins over a field of the same name: the nearer
                    // binding is what a reader expects, and it is what lets a
                    // method take a parameter named like a field.
                    None => match self.implicit_field(ctx, &name) {
                        Some(expr) => expr,
                        None => {
                            self.emit(span, "KSEM060", format!("undefined name `{name}`"));
                            self.program.exprs.alloc(HirExpr::Error)
                        }
                    },
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
                                self.type_name(operand_ty)
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
                                self.type_name(lt),
                                self.type_name(rt)
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
            Expr::StructLit {
                name,
                name_span,
                fields,
                ..
            } => self.analyze_struct_literal(ctx, name, name_span, &fields),
            Expr::Field {
                base,
                field,
                field_span,
                ..
            } => {
                let base_hir = self.analyze_expr(ctx, base);
                let base_ty = self.program.expr(base_hir).type_of();
                let name = self.interner.resolve(field).to_owned();
                match self.resolve_field(base_ty, &name, field_span) {
                    Some((index, ty)) => self.program.exprs.alloc(HirExpr::Field {
                        base: base_hir,
                        index,
                        ty,
                    }),
                    None => self.program.exprs.alloc(HirExpr::Error),
                }
            }
            Expr::MethodCall {
                receiver,
                method,
                method_span,
                args,
                ..
            } => self.analyze_method_call(ctx, receiver, method, method_span, &args),
            Expr::Error { .. } => self.program.exprs.alloc(HirExpr::Error),
        }
    }

    /// Resolves a bare name against the receiver's fields, for a method body
    /// that writes `step` rather than `self.step`.
    ///
    /// Returns `None` outside a method, or when the struct has no such field,
    /// so the caller still reports an undefined name.
    fn implicit_field(&mut self, ctx: &FnCtx, name: &str) -> Option<HirExprId> {
        let owner = ctx.receiver?;
        let receiver = ctx.resolve("self")?;
        let def = self.program.structs.get(owner)?;
        let index = def.field_index(name)?;
        let ty = def.field(index)?.ty;
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Struct(owner),
        });
        Some(self.program.exprs.alloc(HirExpr::Field { base, index, ty }))
    }

    /// Type-checks `receiver.method(args)`.
    ///
    /// A method call is an ordinary call whose first argument is the receiver.
    /// Resolving it to that here is what keeps methods out of the IR and out of
    /// every backend: nothing downstream of analysis knows they exist.
    fn analyze_method_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: kira_source::Span,
        args: &[ExprId],
    ) -> HirExprId {
        let receiver_hir = self.analyze_expr(ctx, receiver);
        let receiver_ty = self.program.expr(receiver_hir).type_of();
        let mut all_args = vec![receiver_hir];
        all_args.extend(args.iter().map(|&arg| self.analyze_expr(ctx, arg)));

        // An error receiver already spoke; do not pile on.
        if receiver_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let name = self.interner.resolve(method).to_owned();
        let Type::Struct(_) = receiver_ty else {
            self.emit(
                method_span,
                "KSEM096",
                format!(
                    "type `{}` has no methods, so it has no method `{name}`",
                    self.type_name(receiver_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let qualified = format!("{}.{name}", self.type_name(receiver_ty));
        if self.lookup_function(&qualified).is_none() {
            // A field holding a value is not callable, and saying so names the
            // likelier mistake than "no such method".
            let message = match self.resolve_field_quietly(receiver_ty, &name) {
                true => format!(
                    "`{name}` is a field of `{}`, not a method",
                    self.type_name(receiver_ty)
                ),
                false => format!(
                    "struct `{}` has no method `{name}`",
                    self.type_name(receiver_ty)
                ),
            };
            self.emit(method_span, "KSEM097", message);
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.analyze_user_call(&qualified, &all_args, method_span)
    }

    /// Type-checks a struct literal into a [`HirExpr::StructNew`] holding one
    /// initializer per declared field, in declaration order.
    ///
    /// A field the literal omits is filled from its declared default, so
    /// nothing downstream of analysis has to know that defaults exist. A field
    /// with neither an initializer nor a default is the one case that cannot be
    /// filled, and it is reported here.
    fn analyze_struct_literal(
        &mut self,
        ctx: &mut FnCtx,
        name: kira_core::Symbol,
        name_span: kira_source::Span,
        inits: &[FieldInit],
    ) -> HirExprId {
        let struct_name = self.interner.resolve(name).to_owned();
        let Some(id) = self.program.structs.lookup(&struct_name) else {
            // A function of this name is the likely mistake, so say which.
            let message = if self.lookup_function(&struct_name).is_some() {
                format!("`{struct_name}` is a function, not a struct")
            } else {
                format!("unknown struct `{struct_name}`")
            };
            self.emit(name_span, "KSEM092", message);
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let field_count = self
            .program
            .structs
            .get(id)
            .map_or(0, |def| def.fields.len());

        // Analyze each written initializer against the field it names, keeping
        // source order so diagnostics read in the order they were written.
        let mut slots: Vec<Option<HirExprId>> = vec![None; field_count];
        for init in inits {
            let field_name = self.interner.resolve(init.name).to_owned();
            let value = self.analyze_expr(ctx, init.value);
            let value_ty = self.program.expr(value).type_of();
            let Some((index, field_ty)) =
                self.resolve_field(Type::Struct(id), &field_name, init.name_span)
            else {
                continue;
            };
            if slots[index as usize].is_some() {
                self.emit(
                    init.name_span,
                    "KSEM093",
                    format!("field `{field_name}` is initialized twice"),
                );
                continue;
            }
            if !value_ty.assignable_to(field_ty) {
                self.emit(
                    init.span,
                    "KSEM094",
                    format!(
                        "field `{field_name}` of `{struct_name}` expects `{}`, found `{}`",
                        self.type_name(field_ty),
                        self.type_name(value_ty)
                    ),
                );
            }
            slots[index as usize] = Some(value);
        }

        // Fill what the literal left out.
        let mut fields = Vec::with_capacity(field_count);
        let mut missing: Vec<String> = Vec::new();
        for index in 0..field_count as u32 {
            if let Some(value) = slots[index as usize] {
                fields.push(value);
                continue;
            }
            match self.field_default(id, index) {
                Some(default) => {
                    let value = self.analyze_default(default);
                    fields.push(value);
                }
                None => {
                    let field_name = self
                        .program
                        .structs
                        .get(id)
                        .and_then(|def| def.field(index))
                        .map_or_else(String::new, |field| field.name.clone());
                    missing.push(field_name);
                    fields.push(self.program.exprs.alloc(HirExpr::Error));
                }
            }
        }
        if !missing.is_empty() {
            self.emit(
                name_span,
                "KSEM095",
                format!(
                    "`{struct_name}` is missing {}: {} (no default is declared)",
                    if missing.len() == 1 {
                        "field"
                    } else {
                        "fields"
                    },
                    missing.join(", ")
                ),
            );
        }
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }

    /// Analyzes a field's default initializer at a construction site.
    ///
    /// Deliberately analyzed in an empty scope rather than the construction
    /// site's: a default belongs to the declaration, so it must not be able to
    /// see whatever locals happen to be in scope wherever the struct is built.
    fn analyze_default(&mut self, default: ExprId) -> HirExprId {
        let mut empty = FnCtx::new(Type::Void);
        self.analyze_expr(&mut empty, default)
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
                    format!(
                        "`print` cannot format a value of type `{}`",
                        self.type_name(arg_ty)
                    ),
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
                            self.type_name(expected),
                            self.type_name(actual)
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
