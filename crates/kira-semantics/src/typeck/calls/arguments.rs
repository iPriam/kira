use super::*;

use kira_semantics_model::OwnershipMode;

impl Analyzer<'_> {
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
        self.analyze_user_call_from_syntax_with_type_args(ctx, name, leading, &[], args, &[], span)
    }

    /// [`Analyzer::analyze_user_call_from_syntax`], plus arguments that occupy
    /// the *last* parameter slots and are already analyzed.
    ///
    /// A construction's trailing children are the case: they fill an `init`'s
    /// content parameter, and no written expression stands for them, so they
    /// arrive as a value rather than as syntax to check an ownership mode
    /// against.
    pub(crate) fn analyze_user_call_from_syntax_with(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[CallArg],
        trailing: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        self.analyze_user_call_from_syntax_with_type_args(
            ctx,
            name,
            leading,
            &[],
            args,
            trailing,
            span,
        )
    }

    /// The syntax-call path with explicit generic type arguments. Keeping the
    /// old wrappers above means all non-generic callers retain their compact
    /// call sites while a `f<Int>(value)` can feed its type list into inference.
    pub(crate) fn analyze_user_call_from_syntax_with_type_args(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        leading: &[HirExprId],
        type_args: &[kira_syntax_model::ast::TypeRefId],
        args: &[CallArg],
        trailing: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let (id, params) =
            match self.resolve_call_target(ctx, name, leading, type_args, args, trailing) {
                CallTarget::Chosen(id, params) => (id, params),
                // Nothing decides which declaration this call means, so it has no
                // meaning. The arguments are still analyzed so their own mistakes
                // are reported beside the one about the call.
                CallTarget::Ambiguous(winners) => {
                    let list = self.overload_list(&winners);
                    self.emit(
                        span,
                        "KSEM275",
                        format!("this call of `{name}` fits {list} equally well"),
                    );
                    for arg in args {
                        self.analyze_expr(ctx, arg.value);
                    }
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                // No signature to check against: still analyze every argument so
                // the mistakes inside them are reported alongside the bad call. A
                // label cannot bind to a callee that does not exist, so it is
                // dropped here and `analyze_user_call` reports the missing function.
                CallTarget::Unknown => {
                    let mut all = leading.to_vec();
                    all.extend(args.iter().map(|arg| self.analyze_expr(ctx, arg.value)));
                    return self.analyze_user_call(name, &all, span);
                }
                CallTarget::Invalid => {
                    for arg in args {
                        self.analyze_expr(ctx, arg.value);
                    }
                    return self.program.exprs.alloc(HirExpr::Error);
                }
            };
        // Arguments bind by position; a label on one is decorative. See
        // `super::labels` for the measurement behind that.
        let positional = Self::argument_slots(args);
        let ownership = self.param_ownership(id);
        let mut all = leading.to_vec();
        // Where each `borrow mut` argument's final value has to land. Collected
        // while the arguments are bound, because that is the only point where a
        // parameter slot and the *syntax* the caller wrote for it are both in
        // hand; attached once the call node exists.
        let mut writebacks: Vec<HirWriteback> = Vec::new();
        for (index, slot_value) in positional.into_iter().enumerate() {
            let slot = index + leading.len();
            // A slot no argument filled takes its parameter's default, when one
            // was declared; otherwise the missing-argument diagnostic already
            // spoke (labeled) or the arity check will (positional), so stand in
            // with an error value that keeps the arity honest and cascades no
            // further.
            let Some(arg) = slot_value else {
                let filled = self
                    .resolve_param_default(id, slot)
                    .unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error));
                all.push(filled);
                continue;
            };
            // An arity mismatch leaves some argument with no parameter to
            // check against. `analyze_user_call` reports the count; here the
            // argument just analyzes plainly rather than being checked against
            // a mode that does not exist.
            match (params.get(slot), ownership.get(slot)) {
                (Some(&expected), Some(&mode)) => {
                    all.push(self.analyze_call_argument(ctx, arg, expected, mode, name));
                    if mode == OwnershipMode::BorrowMut {
                        self.record_borrow_mut_argument(
                            ctx,
                            arg,
                            slot,
                            slot as u32,
                            name,
                            &mut writebacks,
                        );
                    }
                }
                _ => all.push(self.analyze_expr(ctx, arg)),
            }
        }
        // Arguments the caller already analyzed take the slots after the written
        // ones, before any default is reached for.
        all.extend_from_slice(trailing);
        // A positional call that omitted trailing arguments fills them from
        // their defaults, left to right, stopping at the first parameter that
        // declares none — a genuine shortfall the arity check then reports.
        //
        // A `borrow mut` parameter is never filled this way: a default is a
        // value, and there is nowhere in the caller to write one back, so the
        // shortfall is reported instead.
        while all.len() < params.len() {
            if ownership.get(all.len()) == Some(&OwnershipMode::BorrowMut) {
                break;
            }
            match self.resolve_param_default(id, all.len()) {
                Some(default) => all.push(default),
                None => break,
            }
        }
        // The declaration was already chosen, from the argument types as
        // written. Re-choosing here would see them *after* each was coerced
        // into the parameter it was checked against, which is a rubber stamp
        // rather than a second opinion.
        let call = self.analyze_user_call_hinted(name, &all, span, Some(id));
        for writeback in writebacks {
            self.add_writeback(call, writeback);
        }
        call
    }
}
