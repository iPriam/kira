//! Expression type-checking and operator resolution.
//!
//! Each expression is lowered to a typed [`HirExpr`]. Operators resolve to
//! type-specific HIR variants (e.g. `+` on two `Int`s becomes `AddInt`), so no
//! backend re-derives operand types. Any operand that already analyzed to
//! `Error` short-circuits to another `Error`, suppressing cascades.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirExpr, HirExprId};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, FieldInit};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::{resolve_binary, resolve_unary, unary_spelling, unify_branches};

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
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                match ctx.resolve(&name) {
                    Some(local) => {
                        // Reading a moved-out local is the first of KSEM107's
                        // three messages, and it is checked here — at the one
                        // place every read of a local passes through — rather
                        // than at each construct that might contain one.
                        if !self.check_local_live(ctx, local, span) {
                            return self.program.exprs.alloc(HirExpr::Error);
                        }
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
                if name == "print" {
                    // `print` borrows: it renders its argument and consumes
                    // nothing the caller could miss.
                    let arg_hirs: Vec<HirExprId> = args
                        .iter()
                        .map(|&arg| self.analyze_expr(ctx, arg))
                        .collect();
                    self.analyze_print(&arg_hirs, callee_span)
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
                ..
            } => {
                let base_hir = self.analyze_expr(ctx, base);
                let base_ty = self.program.expr(base_hir).type_of();
                let name = self.interner.resolve(field).to_owned();
                // An array has no fields, but it does have `.count` — a
                // property, written with the same syntax a field read uses.
                if base_ty.is_array() {
                    return self.analyze_array_property(base_hir, &name, field_span);
                }
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
        // Analyzing the receiver is how its type is known, and its type is what
        // decides which surface the call belongs to. For an array that is all
        // this pass is for: `append` needs the receiver as a *place*, which is
        // resolved from the syntax, not from the analyzed value.
        //
        // So the diagnostics are marked first and rolled back on the array
        // path, and the place resolution reports on its own. That keeps
        // `resolve_place` the single source of truth for what a bad receiver
        // says, instead of this pass and that one each having an opinion —
        // `grid[nope].append(1)` reports the undefined name exactly once.
        //
        // The probe is *effectful*, so its ownership effects are rolled back
        // too: analyzing `(move xs).append(1)`'s receiver marks `xs` moved, and
        // leaving that in place would report a phantom use-after-move on a later
        // `xs`. The array path re-resolves the receiver from syntax anyway, so
        // the probe's move is undone before it runs.
        let mark = self.diagnostics.len();
        let ownership = ctx.ownership_snapshot();
        let receiver_hir = self.analyze_expr(ctx, receiver);
        let receiver_ty = self.program.expr(receiver_hir).type_of();

        if receiver_ty.is_array() {
            self.diagnostics.truncate(mark);
            ctx.restore_ownership(ownership);
            let name = self.interner.resolve(method).to_owned();
            return self.analyze_array_method(ctx, receiver, &name, method_span, args);
        }

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
        self.analyze_user_call_from_syntax(ctx, &qualified, &[receiver_hir], args, method_span)
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
        let Some(id) = self.program.types.structs().lookup(&struct_name) else {
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
            .types
            .structs()
            .get(id)
            .map_or(0, |def| def.fields.len());

        // Analyze each written initializer against the field it names, keeping
        // source order so diagnostics read in the order they were written.
        let mut slots: Vec<Option<HirExprId>> = vec![None; field_count];
        for init in inits {
            let field_name = self.interner.resolve(init.name).to_owned();
            // The field is resolved before its value, so the field's type is
            // the value's expected type: `H { values = [] }` needs it.
            let resolved = self.resolve_field(Type::Struct(id), &field_name, init.name_span);
            let value = self.analyze_expr_expecting(ctx, init.value, resolved.map(|(_, ty)| ty));
            let value_ty = self.program.expr(value).type_of();
            let Some((index, field_ty)) = resolved else {
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
                    let declared = self
                        .program
                        .types
                        .structs()
                        .get(id)
                        .and_then(|def| def.field(index))
                        .map(|field| field.ty);
                    let value = self.analyze_default(default, declared);
                    fields.push(value);
                }
                None => {
                    let field_name = self
                        .program
                        .types
                        .structs()
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
    pub(crate) fn analyze_default(&mut self, default: ExprId, declared: Option<Type>) -> HirExprId {
        let mut empty = FnCtx::new(Type::Void);
        // The member's declared type is the default's expected type, so
        // `struct H { var values: [Int] = [] }` knows what `[]` holds.
        self.analyze_expr_expecting(&mut empty, default, declared)
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

    /// Type-checks a call whose arguments are still syntax.
    ///
    /// Ownership is the reason this exists. A parameter's [`OwnershipMode`]
    /// decides whether its argument must say `move`, may not say `move`, or
    /// must say `copy` — and answering that needs the *written* argument, not
    /// the analyzed one: `f(mesh)` and `f(move mesh)` produce the same HIR and
    /// differ only in what the source said. So each argument is analyzed
    /// against the mode its parameter declared, and only then handed to
    /// [`Analyzer::analyze_user_call`] for the type check.
    ///
    /// `leading` carries arguments already analyzed — a method's receiver —
    /// which occupy the first parameter slots.
    pub(crate) fn analyze_user_call_from_syntax(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[ExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let sig = self
            .lookup_function(name)
            .map(|(id, params, _)| (id, params.to_vec()));
        let Some((id, params)) = sig else {
            // No signature to check against: still analyze every argument so
            // the mistakes inside them are reported alongside the bad call.
            let mut all = leading.to_vec();
            all.extend(args.iter().map(|&arg| self.analyze_expr(ctx, arg)));
            return self.analyze_user_call(name, &all, span);
        };
        let ownership = self.param_ownership(id);
        let mut all = leading.to_vec();
        for (index, &arg) in args.iter().enumerate() {
            let slot = index + leading.len();
            // An arity mismatch leaves some argument with no parameter to
            // check against. `analyze_user_call` reports the count; here the
            // argument just analyzes plainly rather than being checked against
            // a mode that does not exist.
            match (params.get(slot), ownership.get(slot)) {
                (Some(&expected), Some(&mode)) => {
                    all.push(self.analyze_call_argument(ctx, arg, expected, mode, name));
                }
                _ => all.push(self.analyze_expr(ctx, arg)),
            }
        }
        self.analyze_user_call(name, &all, span)
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
