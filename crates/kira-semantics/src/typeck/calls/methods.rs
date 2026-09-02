use super::*;

impl Analyzer<'_> {
    /// Type-checks `receiver.method(args)`.
    ///
    /// A method call is an ordinary call whose first argument is the receiver.
    /// Resolving it to that here is what keeps methods out of the IR and out of
    /// every backend: nothing downstream of analysis knows they exist.
    ///
    /// `expected` is carried only for the one shape that is not a call at all:
    /// `Result.Ok(1)` parses as a method call, and the instantiation it
    /// constructs comes from the position rather than from anything written.
    pub(in crate::typeck) fn analyze_method_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: Span,
        content: MethodCallContent<'_>,
        expected: Option<Type>,
    ) -> HirExprId {
        let MethodCallContent { args, children } = content;
        if let Some(call) =
            self.analyze_main_thread_namespace(ctx, receiver, method, method_span, args, children)
        {
            return call;
        }
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
        match self.qualified_enum_at(ctx, receiver, expected) {
            crate::enums::QualifiedEnum::Enum(enum_id) => {
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
            // `Result.Ok(1)` where the position asks for no instantiation of
            // `Result`. The receiver is a template, not a value.
            crate::enums::QualifiedEnum::Unanchored(template) => {
                let values = Some(Self::argument_values(args));
                return self.report_unanchored_generic_construction(
                    ctx,
                    &template,
                    method,
                    &values,
                    method_span,
                    expected,
                );
            }
            crate::enums::QualifiedEnum::NotAnEnum => {}
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
        let extractions = self.drop_extraction_snapshot();
        let receiver_hir = self.analyze_expr(ctx, receiver);
        let receiver_ty = self.program.expr(receiver_hir).type_of();
        let name = self.interner.resolve(method).to_owned();
        let qualified = format!("{}.{name}", self.member_owner_name(receiver_ty));
        let has_user_method = self.lookup_function(&qualified).is_some();

        if receiver_ty.is_array() {
            if has_user_method && !matches!(name.as_str(), "append" | "count") {
                return self.analyze_resolved_method(
                    ctx,
                    receiver,
                    receiver_hir,
                    &qualified,
                    MethodCallContent { args, children },
                    method_span,
                );
            }
            self.diagnostics.truncate(mark);
            ctx.restore_ownership(ownership);
            self.restore_drop_extractions(extractions);
            // An array builtin binds its argument by shape, not by a parameter
            // name, so a label on one is a mistake.
            let values = Self::argument_values(args);
            return self.analyze_array_method(ctx, receiver, &name, method_span, &values);
        }
        if let Type::Enum(existential_id) = receiver_ty
            && self.is_trait_existential_type(existential_id)
        {
            return self.analyze_trait_existential_call(
                ctx,
                receiver_hir,
                existential_id,
                &name,
                crate::constructs::ConstructCallContent {
                    args,
                    children,
                    receiver_syntax: Some(receiver),
                },
                method_span,
            );
        }
        if let Type::Enum(family_id) = receiver_ty
            && self.is_construct_family_type(family_id)
        {
            return self.analyze_construct_family_call(
                ctx,
                receiver_hir,
                family_id,
                &name,
                crate::constructs::ConstructCallContent {
                    args,
                    children,
                    receiver_syntax: Some(receiver),
                },
                method_span,
            );
        }

        // A task handle's three operations are matched before anything tries to
        // resolve a method on it: it is not a struct, so a field-based lookup
        // would report a missing type rather than the opaque-handle rule.
        if matches!(receiver_ty, Type::Task(_)) {
            return self.analyze_task_method(ctx, receiver_hir, &name, args, method_span);
        }
        if matches!(receiver_ty, Type::MainThreadTask(_)) {
            return self.analyze_main_thread_task_method(
                ctx,
                receiver_hir,
                &name,
                args,
                method_span,
            );
        }
        if receiver_ty == Type::String {
            if has_user_method && !is_builtin_string_method(&name) {
                return self.analyze_resolved_method(
                    ctx,
                    receiver,
                    receiver_hir,
                    &qualified,
                    MethodCallContent { args, children },
                    method_span,
                );
            }
            // A string builtin binds its arguments by shape, not by a parameter
            // name, so a label on one is a mistake.
            let values = Self::argument_values(args);
            return self.analyze_string_method(ctx, receiver_hir, &name, method_span, &values);
        }
        // An error receiver already spoke; do not pile on.
        if receiver_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if self.refuse_direct_drop_call(receiver_ty, &name, method_span) {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // A field of function type is *called* through this same syntax, so a
        // closure in a field is tried before a method of that name is looked
        // up. A method wins if both exist, because a method is what the
        // receiver's type declares and a field only what it stores.
        if !has_user_method && let Type::Struct(_) = receiver_ty {
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
                return call;
            }
        }
        if !has_user_method {
            // A concrete construct value calls its family's `extend` modifiers
            // through the same syntax: the receiver has no method of this name,
            // but its family does. Upcast the receiver into the family value and
            // dispatch there — `Text(…).padding(8)` becomes a family call.
            if let Type::Struct(owner) = receiver_ty
                && let Some(family_id) = self.family_uniform_method(owner, &name)
            {
                let upcast = self.coerce_construct_value(receiver_hir, Some(Type::Enum(family_id)));
                return self.analyze_construct_family_call(
                    ctx,
                    upcast,
                    family_id,
                    &name,
                    crate::constructs::ConstructCallContent {
                        args,
                        children,
                        receiver_syntax: Some(receiver),
                    },
                    method_span,
                );
            }
            if let Type::Struct(owner) = receiver_ty
                && self.report_ambiguous_member(owner, &name, method_span, true)
            {
                return self.program.exprs.alloc(HirExpr::Error);
            }
            // A field holding a value is not callable, and saying so names the
            // likelier mistake than "no such method".
            if let Type::Struct(_) = receiver_ty {
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
            } else {
                self.emit(
                    method_span,
                    "KSEM096",
                    format!(
                        "type `{}` has no methods, so it has no method `{name}`",
                        self.type_name(receiver_ty)
                    ),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.analyze_resolved_method(
            ctx,
            receiver,
            receiver_hir,
            &qualified,
            MethodCallContent { args, children },
            method_span,
        )
    }

    /// Type-checks a method whose concrete receiver already selected a user
    /// callable. Trait defaults use this path for every type an `extend` block
    /// may target, including scalars, arrays, strings, and concrete enums.
    fn analyze_resolved_method(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        receiver_hir: HirExprId,
        qualified: &str,
        content: MethodCallContent<'_>,
        method_span: Span,
    ) -> HirExprId {
        let MethodCallContent { args, children } = content;
        // A trailing block on a struct method fills that method's content
        // parameter, exactly as it does on a construction or a family modifier.
        // The children are already analyzed into one value, so they occupy the
        // last parameter slot rather than arriving as written arguments.
        let trailing = match self.lookup_function(qualified).map(|(id, _, _)| id) {
            Some(id) if !children.is_empty() => match self.init_content_param(id) {
                Some(content) => vec![self.content_value(ctx, &content, children, method_span)],
                None => {
                    for &child in children {
                        self.analyze_expr(ctx, child);
                    }
                    self.emit(
                        method_span,
                        "KSEM278",
                        format!(
                            "`{qualified}` takes no trailing content; give it a last parameter                              of `some X` for one child or `[some X]` for a list of them"
                        ),
                    );
                    Vec::new()
                }
            },
            _ => Vec::new(),
        };
        let call = self.analyze_user_call_from_syntax_with(
            ctx,
            qualified,
            &[receiver_hir],
            args,
            &trailing,
            method_span,
        );
        // A method that does not mutate borrows its receiver, so a member read
        // filling one is not a second owner. A mutating method is the opposite:
        // it takes the receiver by value and writes it back, which for a value
        // that runs a user `Drop` is exactly the second owner this refuses.
        if !self.callee_mutates(call) {
            self.excuse_drop_extraction(receiver_hir);
        }
        // When the method mutates its receiver, the written receiver is resolved
        // as a mutable place so the mutation lands back in the caller's storage.
        self.record_mut_receiver(ctx, call, receiver);
        call
    }
}

/// Whether `name` belongs to `String`'s built-in method surface.
fn is_builtin_string_method(name: &str) -> bool {
    matches!(name, "count" | "charAt" | "indexOf" | "substring")
        || kira_runtime_abi::StringOp::from_method_name(name).is_some()
}
