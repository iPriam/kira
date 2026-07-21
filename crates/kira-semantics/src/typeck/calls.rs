//! Calls and construction: every expression that names something and hands it
//! arguments.
//!
//! Split out of [`super`] because it is a cohesive surface with one shared
//! question — *what is being called, and does the argument list fit its
//! signature* — and because four of the five kinds here share the argument
//! checking that [`Analyzer::analyze_user_call`] does. A method call, a
//! module-qualified call, and a bare call all end up there; a struct literal
//! and `print` are the two that do not, and they are the two that are not
//! calls to a user function.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirExpr, HirExprId};
use kira_syntax_model::ast::{CallArg, Expr, ExprId, FieldInit};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Type-checks `receiver.method(args)`.
    ///
    /// A method call is an ordinary call whose first argument is the receiver.
    /// Resolving it to that here is what keeps methods out of the IR and out of
    /// every backend: nothing downstream of analysis knows they exist.
    pub(super) fn analyze_method_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: kira_source::Span,
        args: &[CallArg],
    ) -> HirExprId {
        // `Support.hello()` is a *module-qualified free call*, not a method
        // call, and it is recognized here because the parser cannot tell the
        // two apart: both are `<expr> . name ( args )`. What separates them is
        // that the receiver is a bare name which is no local and which this
        // file imported as a module — a question only the analyzer, holding the
        // file-scoped import table, can answer.
        if let Some(call) = self.analyze_qualified_call(ctx, receiver, method, method_span, args) {
            return call;
        }
        // `SizeMode.Fixed(3)` / `Foundation.SizeMode.Fixed(3)` is a
        // payload-carrying enum variant written with a qualified spelling: the
        // receiver names the enum, `method` names the variant, and the argument
        // is its payload. It parses as a method call because the parser cannot
        // tell an enum name from a value; the analyzer, holding the enum table
        // and the import table, can.
        if let Some(enum_id) = self.qualified_enum_at(ctx, receiver) {
            self.reject_argument_labels(args, "an enum variant payload");
            let values = Some(Self::argument_values(args));
            return self.analyze_dot_member(
                ctx,
                method,
                method_span,
                &values,
                method_span,
                Some(Type::Enum(enum_id)),
            );
        }

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
            // An array builtin binds its argument by shape, not by a parameter
            // name, so a label on one is a mistake.
            self.reject_argument_labels(args, "an array method");
            let name = self.interner.resolve(method).to_owned();
            let values = Self::argument_values(args);
            return self.analyze_array_method(ctx, receiver, &name, method_span, &values);
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
        // A field of function type is *called* through this same syntax, so a
        // closure in a field is tried before a method of that name is looked
        // up. A method wins if both exist, because a method is what the
        // receiver's type declares and a field only what it stores.
        let qualified = format!("{}.{name}", self.type_name(receiver_ty));
        if self.lookup_function(&qualified).is_none() {
            let values = Self::argument_values(args);
            if let Some(call) = self.analyze_field_closure_call(
                ctx,
                receiver_hir,
                receiver_ty,
                &name,
                &values,
                method_span,
            ) {
                // A closure stored in a field exposes no parameter names here.
                self.reject_argument_labels(args, "a call through a function value");
                return call;
            }
        }
        if self.lookup_function(&qualified).is_none() {
            if let Type::Struct(owner) = receiver_ty
                && self.report_ambiguous_member(owner, &name, method_span, true)
            {
                return self.program.exprs.alloc(HirExpr::Error);
            }
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
    pub(super) fn analyze_struct_literal(
        &mut self,
        ctx: &mut FnCtx,
        name: kira_core::Symbol,
        name_span: kira_source::Span,
        inits: &[FieldInit],
    ) -> HirExprId {
        // A module-qualified literal (`Support.Point { … }`) names the same
        // struct the module declares bare: stripping the qualifier against this
        // file's imports is the whole of it, exactly as a qualified *type*
        // reference is resolved. An unimported root is reported there and the
        // literal's own fields are still analyzed so their mistakes surface.
        let written = self.interner.resolve(name).to_owned();
        let Some(struct_name) = self.strip_module_qualifier(&written, name_span) else {
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
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
        self.link_type_name(&struct_name, name_span);
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
            if resolved.is_some() {
                self.link_field_name(&struct_name, &field_name, init.name_span);
            }
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

        // Fill what the literal left out. A `@FFI.Struct { layout: c }` starts
        // from a zeroed value, so an omitted field with no default takes its
        // zero rather than being reported missing — the oracle's construction
        // rule.
        let is_c_layout = self.ffi_c_layout_named(&struct_name).is_some();
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
                None if is_c_layout => {
                    fields.push(self.ffi_zero_field(id, index, name_span));
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

    pub(super) fn analyze_print(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
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

    /// Resolves `Root.name(args)` when `Root` is a module this file imported.
    ///
    /// Returns `None` when the shape is not a module-qualified call at all, so
    /// the caller carries on as a method call. It returns `Some(Error)` — not
    /// `None` — when `Root` is a module the *program* has but this file did
    /// not import: that is a real mistake with a real diagnostic, and falling
    /// through to "type `Error` has no methods" would bury it.
    fn analyze_qualified_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: kira_source::Span,
        args: &[CallArg],
    ) -> Option<HirExprId> {
        let Expr::Name { symbol, span } = *self.tree.expr(receiver) else {
            return None;
        };
        let root = self.interner.resolve(symbol).to_owned();
        // A local of the same name wins: a binding the reader can see beats a
        // module they have to look up, and it is what keeps a module name from
        // becoming unusable as a variable.
        if ctx.resolve(&root).is_some() {
            return None;
        }
        // `ClsAccount.gross()` inside a subclass method: the parent's body run
        // against `self`. Checked before the module table, because a parent
        // type name is nearer than a module name.
        if self.program.types.structs().lookup(&root).is_some() {
            // Once the root is known to be a type name, this path owns the
            // call: falling through would analyze the type name as a value and
            // report it undefined on top of whatever was already said.
            let Some(qualifier) = self.resolve_parent_qualifier(ctx, &root, span) else {
                for arg in args {
                    self.analyze_expr(ctx, arg.value);
                }
                return Some(self.program.exprs.alloc(HirExpr::Error));
            };
            let name = self.interner.resolve(method).to_owned();
            return Some(self.analyze_parent_call(ctx, qualifier, &name, args, method_span));
        }
        if self.module_for_root(&root).is_some() {
            let name = self.interner.resolve(method).to_owned();
            return Some(self.analyze_user_call_from_syntax(ctx, &name, &[], args, method_span));
        }
        if self.report_unimported_root(&root, span) {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        None
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
        args: &[CallArg],
        span: kira_source::Span,
    ) -> HirExprId {
        let sig = self
            .lookup_function(name)
            .map(|(id, params, _)| (id, params.to_vec()));
        let Some((id, params)) = sig else {
            // No signature to check against: still analyze every argument so
            // the mistakes inside them are reported alongside the bad call. A
            // label cannot bind to a callee that does not exist, so it is
            // dropped here and `analyze_user_call` reports the missing function.
            let mut all = leading.to_vec();
            all.extend(args.iter().map(|arg| self.analyze_expr(ctx, arg.value)));
            return self.analyze_user_call(name, &all, span);
        };
        // A labeled call is resolved to declaration order first, so the
        // ownership and type checks below see the same positional list an
        // unlabeled call produces.
        let param_names = self.param_names(id);
        let positional = self.bind_call_arguments(args, leading.len(), &param_names, name, span);
        let ownership = self.param_ownership(id);
        let mut all = leading.to_vec();
        for (index, slot_value) in positional.into_iter().enumerate() {
            let slot = index + leading.len();
            // A labeled call that named no argument for this parameter left it
            // empty; the missing-argument diagnostic already spoke, so stand in
            // with an error value that keeps the arity honest and cascades no
            // further.
            let Some(arg) = slot_value else {
                all.push(self.program.exprs.alloc(HirExpr::Error));
                continue;
            };
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
        self.link_function(id, span);
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
