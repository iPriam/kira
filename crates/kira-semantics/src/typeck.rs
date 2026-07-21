//! Expression type-checking and operator resolution.
//!
//! Each expression is lowered to a typed [`HirExpr`]. Operators resolve to
//! type-specific HIR variants (e.g. `+` on two `Int`s becomes `AddInt`), so no
//! backend re-derives operand types. Any operand that already analyzed to
//! `Error` short-circuits to another `Error`, suppressing cascades.
//!
//! Calls and construction live in [`calls`]: they share one question — what is
//! being called, and does the argument list fit its signature — and all but two
//! of them end up in the same argument checker.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::classes::Qualifier;
use crate::operators::{resolve_binary, resolve_unary, unary_spelling, unify_branches};

mod calls;
mod labels;

impl Analyzer<'_> {
    /// Type-checks an AST expression, returning its HIR handle.
    pub(crate) fn analyze_expr(&mut self, ctx: &mut FnCtx, id: ExprId) -> HirExprId {
        self.analyze_expr_expecting(ctx, id, None)
    }

    /// Type-checks an expression that sits where `expected` is wanted.
    ///
    /// The hint exists for exactly one construct: an **empty array literal**
    /// has no element to infer a type from, so `var xs: [Int] = []` can only
    /// work if the position's type reaches the literal. Every other expression
    /// ignores it and is typed bottom-up as before — this is a hint, not
    /// bidirectional type checking, and widening it into one would be a much
    /// larger change than the one construct that needs it.
    ///
    /// `None` means "nothing is expected here", which is different from
    /// expecting `Error`: the callers that have a type pass it, and the rest
    /// keep calling [`Analyzer::analyze_expr`].
    pub(crate) fn analyze_expr_expecting(
        &mut self,
        ctx: &mut FnCtx,
        id: ExprId,
        expected: Option<Type>,
    ) -> HirExprId {
        let node = self.tree.expr(id).clone();
        match node {
            Expr::Int { value, .. } => self.program.exprs.alloc(HirExpr::Int(value)),
            Expr::Float { value, .. } => self.program.exprs.alloc(HirExpr::Float(value)),
            Expr::Bool { value, .. } => self.program.exprs.alloc(HirExpr::Bool(value)),
            Expr::Str { value, .. } => self.program.exprs.alloc(HirExpr::Str(value)),
            // `move xs` / `copy xs` sits where its operand sits, so whatever
            // was expected of the transfer is expected of what it transfers.
            Expr::Ownership { op, operand, span } => {
                self.analyze_ownership_expr(ctx, op, operand, span, expected)
            }
            // Reaching a `try` *here* means it is not the whole initializer of a
            // `let` directly inside an `attempt` body — the one position
            // `stmt::attempts` intercepts, and the only one the reference
            // pins. The operand is still analyzed so its own mistakes surface.
            Expr::Try { value, span } => {
                self.analyze_expr(ctx, value);
                self.emit(
                    span,
                    "KSEM137",
                    "`try` is only allowed as the initializer of a `let` directly inside an \
                     `attempt` body"
                        .to_owned(),
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
            Expr::ArrayLit { elements, span } => {
                self.analyze_array_literal(ctx, &elements, span, expected)
            }
            Expr::Index { base, index, span } => self.analyze_index(ctx, base, index, span),
            Expr::DotMember {
                name,
                name_span,
                args,
                span,
            } => self.analyze_dot_member(ctx, name, name_span, &args, span, expected),
            Expr::Closure {
                ref params,
                ref body,
                span,
            } => self.analyze_closure(ctx, params, body, span, expected),
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                // A name that lives in an enclosing closure frame is captured
                // here, on the one path every read of a name passes through.
                match self.resolve_capturing(ctx, &name, span) {
                    crate::closures::Captured::Refused => self.program.exprs.alloc(HirExpr::Error),
                    crate::closures::Captured::Local(local) => {
                        // Reading a moved-out local is the first of KSEM107's
                        // three messages, and it is checked here — at the one
                        // place every read of a local passes through — rather
                        // than at each construct that might contain one.
                        if !self.check_local_live(ctx, local, span) {
                            return self.program.exprs.alloc(HirExpr::Error);
                        }
                        if let Some(binding) = ctx.binding_span(local) {
                            let definition = kira_source::FileSpan::new(self.source, binding);
                            self.link(span, definition);
                        }
                        let ty = ctx.local_type(local);
                        self.program.exprs.alloc(HirExpr::Local { local, ty })
                    }
                    // A local wins over a field of the same name: the nearer
                    // binding is what a reader expects, and it is what lets a
                    // method take a parameter named like a field.
                    crate::closures::Captured::Absent => match self.implicit_field(ctx, &name) {
                        Some(expr) => {
                            // A bare field read inside a method resolves to
                            // the receiver's field, so a jump from it lands on
                            // that field's declaration.
                            if let Some(owner) = ctx.receiver.and_then(|owner| {
                                self.program
                                    .types
                                    .structs()
                                    .get(owner)
                                    .map(|def| def.name.clone())
                            }) {
                                self.link_field_name(&owner, &name, span);
                            }
                            expr
                        }
                        None => {
                            // A name several parents declare is inherited but
                            // unresolvable, which is a different mistake from
                            // one nobody declared — and a different fix.
                            if !ctx.receiver.is_some_and(|owner| {
                                self.report_ambiguous_member(owner, &name, span, false)
                            }) {
                                self.emit(span, "KSEM060", format!("undefined name `{name}`"));
                            }
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
            Expr::Binary { op, lhs, rhs, span } => self.analyze_binary(ctx, op, lhs, rhs, span),
            Expr::Conditional {
                cond,
                then,
                otherwise,
                span,
            } => self.analyze_conditional(ctx, cond, then, otherwise, span, expected),
            Expr::Call {
                callee,
                callee_span,
                args,
                ..
            } => {
                let name = self.interner.resolve(callee).to_owned();
                // The value paths below bind by position, not by parameter
                // name; only a user function or method exposes names to bind a
                // label against. Each of those paths keeps the written values
                // and refuses a label it cannot honor.
                let values = Self::argument_values(&args);
                // A binding of function type is called by naming it, and the
                // binding wins over a function of the same name for the same
                // reason a local wins over a field: the nearer name is the one
                // a reader means.
                if let Some(call) =
                    self.analyze_local_closure_call(ctx, &name, &values, callee_span)
                {
                    self.reject_argument_labels(&args, "a call through a function value");
                    return call;
                }
                // A class is constructed by calling it, so a call whose callee
                // names a class is a constructor, not a function call.
                if let Some(id) = self.class_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    // A constructor fills fields by position; binding them by
                    // name is not supported on this surface yet.
                    self.reject_argument_labels(&args, "a class constructor");
                    return self.analyze_class_new(ctx, id, &values, callee_span);
                }
                // A construct-backed declaration is constructed by calling it,
                // like a class — but its params carry names, so a labeled
                // argument binds to the input of that name.
                if let Some(id) = self.construct_backed_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    return self.analyze_construct_new(ctx, id, &args, callee_span);
                }
                // `T()` on a `@FFI.Struct { layout: c }` is the zeroed-value
                // form: it takes no arguments and every field takes its zero.
                // Field initializers are written `T { field: value }` instead.
                if let Some(id) = self.ffi_c_layout_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    self.reject_argument_labels(&args, "a C-layout struct constructor");
                    if !values.is_empty() {
                        let struct_name = self.program.types.type_name(Type::Struct(id));
                        for &value in &values {
                            self.analyze_expr(ctx, value);
                        }
                        self.emit(
                            callee_span,
                            "KSEM189",
                            format!(
                                "C-layout `{struct_name}` takes no positional arguments: write \
                                 `{struct_name}()` for a zeroed value or `{struct_name} {{ field: \
                                 value }}` to initialize fields"
                            ),
                        );
                        return self.program.exprs.alloc(HirExpr::Error);
                    }
                    return self.ffi_zero_filled_struct(id, callee_span);
                }
                // A bare call inside a method may name one of the receiver's
                // own or inherited methods, the way a bare name may read one of
                // its fields. A method exposes parameter names, so labels flow
                // through unchanged.
                if let Some(call) = self.implicit_method_call(ctx, &name, &args, callee_span) {
                    return call;
                }
                // `Int(x)` / `U32(x)` / `Float(x)` and the rest of the numeric
                // scalar set is a value conversion, not a call — recognized here
                // before the undefined-function path so a cast is never reported
                // as a missing function.
                if let Some(call) = self.analyze_scalar_conversion(ctx, &name, &args, callee_span) {
                    return call;
                }
                if name == "print" {
                    // `print` borrows: it renders its argument and consumes
                    // nothing the caller could miss.
                    self.reject_argument_labels(&args, "the `print` builtin");
                    let arg_hirs: Vec<HirExprId> = values
                        .iter()
                        .map(|&arg| self.analyze_expr(ctx, arg))
                        .collect();
                    self.analyze_print(&arg_hirs, callee_span)
                } else if let Some(id) = self.foreign_named(&name) {
                    // A bare call whose name is a recorded `@FFI.Extern`
                    // callable is an ordinary Kira call — no `@Native`, no
                    // ceremony — resolved to `Callee::Foreign`.
                    self.reject_argument_labels(&args, "a foreign function");
                    self.analyze_foreign_call(ctx, id, &values, callee_span)
                } else {
                    self.analyze_user_call_from_syntax(ctx, &name, &[], &args, callee_span)
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
                span,
            } => {
                let name = self.interner.resolve(field).to_owned();
                // `ClsAlpha.v` reads a parent's field through `self`; the base
                // is a type name, not a value, so it must be recognized before
                // anything tries to analyze it as one.
                match self.parent_qualifier_of(ctx, base) {
                    Qualifier::Parent(qualifier) => {
                        return self.analyze_parent_field(ctx, qualifier, &name, field_span);
                    }
                    // The qualifier was a type name and it did not apply here;
                    // that was already reported, so say nothing more about it.
                    Qualifier::Rejected => {
                        return self.program.exprs.alloc(HirExpr::Error);
                    }
                    Qualifier::NotAType => {}
                }
                // `SizeMode.Hug` / `Foundation.SizeMode.Fill` is a payload-less
                // enum variant written with a qualified spelling rather than a
                // leading dot — the base names the enum, `field` names the
                // variant. Recognized before the base is analyzed as a value,
                // because an enum name is not one.
                if let Some(enum_id) = self.qualified_enum_at(ctx, base) {
                    return self.analyze_dot_member(
                        ctx,
                        field,
                        field_span,
                        &None,
                        span,
                        Some(Type::Enum(enum_id)),
                    );
                }
                let base_hir = self.analyze_expr(ctx, base);
                let base_ty = self.program.expr(base_hir).type_of();
                // An array has no fields, but it does have `.count` — a
                // property, written with the same syntax a field read uses.
                if base_ty.is_array() {
                    return self.analyze_array_property(base_hir, &name, field_span);
                }
                // A construct's computed bridge member (`value.node`) is read as
                // a property but runs the member, so it lowers to a method call
                // rather than a field read.
                if let Type::Struct(id) = base_ty
                    && self.construct_computed_member(id, &name)
                {
                    return self
                        .analyze_construct_bridge_read(ctx, base_hir, id, &name, field_span);
                }
                match self.resolve_field(base_ty, &name, field_span) {
                    Some((index, ty)) => {
                        if let Type::Struct(id) = base_ty
                            && let Some(owner) = self
                                .program
                                .types
                                .structs()
                                .get(id)
                                .map(|def| def.name.clone())
                        {
                            self.link_field_name(&owner, &name, field_span);
                        }
                        self.program.exprs.alloc(HirExpr::Field {
                            base: base_hir,
                            index,
                            ty,
                        })
                    }
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

    /// Type-checks `cond ? then : otherwise`.
    ///
    /// The expected-type hint is forwarded to both branches so an empty array
    /// literal keeps working in either one — `flag ? [] : xs` needs the hint to
    /// reach the `[]` exactly as `var xs: [Int] = []` does. A leading-dot member
    /// resolves the same way: whichever branch types concretely anchors the
    /// other, so `flag ? .Red : tone` and `flag ? tone : .Red` both work.
    fn analyze_conditional(
        &mut self,
        ctx: &mut FnCtx,
        cond: ExprId,
        then: ExprId,
        otherwise: ExprId,
        span: kira_source::Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let cond_hir = self.analyze_expr(ctx, cond);
        let cond_ty = self.program.expr(cond_hir).type_of();
        if cond_ty != Type::Error && cond_ty != Type::Bool {
            let name = self.type_name(cond_ty);
            self.emit(
                self.tree.expr(cond).span(),
                "KSEM131",
                format!("the condition of `? :` must be `Bool`, not `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }

        // As in `analyze_binary`, the concrete branch is analyzed first when the
        // other is a leading dot, so the dot inherits a type to resolve against.
        let then_is_dot = matches!(self.tree.expr(then), Expr::DotMember { .. });
        let otherwise_is_dot = matches!(self.tree.expr(otherwise), Expr::DotMember { .. });
        let (then_hir, otherwise_hir) = if then_is_dot && !otherwise_is_dot {
            let otherwise_hir = self.analyze_expr_expecting(ctx, otherwise, expected);
            let hint = self.program.expr(otherwise_hir).type_of();
            let then_hir = self.analyze_expr_expecting(ctx, then, Some(hint));
            (then_hir, otherwise_hir)
        } else {
            let then_hir = self.analyze_expr_expecting(ctx, then, expected);
            let hint = self.program.expr(then_hir).type_of();
            let otherwise_hir = if otherwise_is_dot {
                self.analyze_expr_expecting(ctx, otherwise, Some(hint))
            } else {
                self.analyze_expr_expecting(ctx, otherwise, expected.or(Some(hint)))
            };
            (then_hir, otherwise_hir)
        };

        let then_ty = self.program.expr(then_hir).type_of();
        let otherwise_ty = self.program.expr(otherwise_hir).type_of();
        if then_ty == Type::Error || otherwise_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let Some(ty) = unify_branches(then_ty, otherwise_ty) else {
            let (then_name, otherwise_name) =
                (self.type_name(then_ty), self.type_name(otherwise_ty));
            self.emit(
                span,
                "KSEM132",
                format!("the branches of `? :` disagree: `{then_name}` and `{otherwise_name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };

        // A `? :` is an expression, so it must produce a value. Two `Void`
        // branches agree on a type but leave nothing on the stack for the
        // surrounding expression to consume, which no backend can lower; it is
        // rejected here rather than reaching one.
        if ty == Type::Void {
            self.emit(
                span,
                "KSEM133",
                "a `? :` expression must produce a value, but both branches are `Void`".to_owned(),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }

        self.program.exprs.alloc(HirExpr::Select {
            cond: cond_hir,
            then: then_hir,
            otherwise: otherwise_hir,
            ty,
        })
    }

    /// Type-checks a binary operation, threading expected types so a
    /// leading-dot operand resolves and desugaring enum equality to a tag
    /// comparison.
    ///
    /// A leading-dot member (`.Red`) has no bottom-up type: it resolves only
    /// against an expected one. So when exactly one operand is a leading dot,
    /// the *other* is analyzed first and its type becomes the dot's expectation
    /// — which is what makes `c == .Red` and `red != .Green` type-check without
    /// bidirectional inference in the general case.
    fn analyze_binary(
        &mut self,
        ctx: &mut FnCtx,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        span: kira_source::Span,
    ) -> HirExprId {
        let lhs_is_dot = matches!(self.tree.expr(lhs), Expr::DotMember { .. });
        let rhs_is_dot = matches!(self.tree.expr(rhs), Expr::DotMember { .. });
        // Analyze the concrete side first when the other is a leading dot, so
        // the dot inherits its type.
        let (lhs_hir, rhs_hir) = if lhs_is_dot && !rhs_is_dot {
            let rhs_hir = self.analyze_expr(ctx, rhs);
            let rt = self.program.expr(rhs_hir).type_of();
            let lhs_hir = self.analyze_expr_expecting(ctx, lhs, Some(rt));
            (lhs_hir, rhs_hir)
        } else {
            let lhs_hir = self.analyze_expr(ctx, lhs);
            let lt = self.program.expr(lhs_hir).type_of();
            let rhs_hir = if rhs_is_dot {
                self.analyze_expr_expecting(ctx, rhs, Some(lt))
            } else {
                self.analyze_expr(ctx, rhs)
            };
            (lhs_hir, rhs_hir)
        };

        let lt = self.program.expr(lhs_hir).type_of();
        let rt = self.program.expr(rhs_hir).type_of();
        if lt == Type::Error || rt == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        // Enum equality is tag equality: `e == .V` becomes an `Int` comparison
        // of two discriminants, so no backend learns enums can be compared.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && matches!(lt, Type::Enum(_)) && lt == rt {
            return self.enum_equality(op == BinaryOp::Eq, lhs_hir, rhs_hir);
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

    /// Resolves a bare name against the receiver's fields, for a method body
    /// that writes `step` rather than `self.step`.
    ///
    /// Returns `None` outside a method, or when the struct has no such field,
    /// so the caller still reports an undefined name.
    fn implicit_field(&mut self, ctx: &FnCtx, name: &str) -> Option<HirExprId> {
        let owner = ctx.receiver?;
        let receiver = ctx.resolve("self")?;
        let def = self.program.types.structs().get(owner)?;
        let index = def.field_index(name)?;
        let ty = def.field(index)?.ty;
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Struct(owner),
        });
        Some(self.program.exprs.alloc(HirExpr::Field { base, index, ty }))
    }

    /// Analyzes a field's default initializer at a construction site.
    ///
    /// Deliberately analyzed in an empty scope rather than the construction
    /// site's: a default belongs to the declaration, so it must not be able to
    /// see whatever locals happen to be in scope wherever the struct is built.
    pub(crate) fn analyze_default(&mut self, default: ExprId, declared: Option<Type>) -> HirExprId {
        let mut empty = FnCtx::new(Type::Void);
        // The member's declared type is the default's expected type, so
        // `struct H { var values: [Int] = [] }` knows what `[]` holds.
        self.analyze_expr_expecting(&mut empty, default, declared)
    }
}
