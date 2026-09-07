//! Calls whose arguments have been analyzed or are still syntax.

use kira_semantics_model::OwnershipMode;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{Callee, FuncId, HirExpr, HirExprId, HirWriteback};
use kira_syntax_model::ast::{CallArg, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;
use crate::typeck::overloads::OverloadFailure;

use super::CallTarget;
use super::math::wrapping_operator;

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
        self.analyze_user_call_from_syntax_with(ctx, name, leading, args, &[], span)
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
        let (id, params) = match self.resolve_call_target(ctx, name, leading, args, trailing) {
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

    /// The declaration a call names, and the parameter types its arguments are
    /// analyzed against.
    ///
    /// A name declared once answers immediately. An overloaded one is resolved
    /// from the types its arguments have on their own, because the parameter a
    /// given argument is checked against is exactly what is being decided —
    /// see [`Analyzer::try_argument_types`].
    pub(super) fn resolve_call_target(
        &mut self,
        ctx: &FnCtx,
        name: &str,
        leading: &[HirExprId],
        args: &[CallArg],
        trailing: &[HirExprId],
    ) -> CallTarget {
        let candidates = self.visible_overloads(name);
        let id = match candidates.as_slice() {
            [] => return CallTarget::Unknown,
            [only] => *only,
            _ => {
                let mut actual = self.try_argument_types(ctx, leading, args);
                actual.extend(
                    trailing
                        .iter()
                        .map(|&value| self.program.expr(value).type_of()),
                );
                match self.resolve_overload(&candidates, &actual) {
                    Ok(id) => id,
                    Err(OverloadFailure::Ambiguous(winners)) => {
                        return CallTarget::Ambiguous(winners);
                    }
                    // A call that fits nothing still needs a signature to be
                    // checked against, so the first declaration speaks and
                    // reports the mismatch against what it expected.
                    Err(OverloadFailure::None) => candidates[0],
                }
            }
        };
        CallTarget::Chosen(id, self.param_types(id))
    }

    /// Resolves the caller storage a `borrow mut` argument names, recording
    /// where the callee's final value has to land.
    ///
    /// A `borrow mut` parameter is the callee writing through the caller's
    /// binding, so the argument has to *be* a binding: a temporary would be
    /// mutated and then discarded, which is why that is refused rather than
    /// silently accepted. Two arguments rooted at the same local are refused
    /// for the same reason in reverse — both writes would land in one place and
    /// the later one would erase the earlier.
    ///
    /// `slot` is the parameter as the *source* numbers it, which is what the
    /// diagnostic names; `param` is the slot on the function actually called,
    /// and the two differ by one through a function value, whose dispatcher
    /// carries the closure itself in slot 0.
    pub(crate) fn record_borrow_mut_argument(
        &mut self,
        ctx: &mut FnCtx,
        arg: ExprId,
        slot: usize,
        param: u32,
        callee: &str,
        writebacks: &mut Vec<HirWriteback>,
    ) {
        let span = self.tree.expr(arg).span();
        let Some((place, _)) = self.resolve_place(ctx, arg, PlacePurpose::BorrowMut) else {
            return;
        };
        if let Some(existing) = writebacks
            .iter()
            .find(|entry| crate::place::places_overlap(&entry.place, &place))
        {
            let name = ctx.local_name(place.local);
            // Reported in the source's numbering, which is the callee's shifted
            // back by however far this call's slot 0 sits from parameter 0.
            let other = existing.param as usize + slot - param as usize;
            self.emit(
                span,
                "KSEM247",
                format!(
                    "`{callee}` mutably borrows the same storage through `{name}` twice in \
                     one call (parameters {other} and {slot}); the two writes would land in \
                     the same place and the later one would erase the earlier"
                ),
            );
            return;
        }
        writebacks.push(HirWriteback { param, place });
    }

    /// The name of the copy specialized for these arguments' concrete classes.
    ///
    /// Built from the arguments rather than looked up by signature because the
    /// specialization *is* named after them — see `Analyzer::callable_name`. An
    /// argument whose type is the declared class contributes nothing, so a call
    /// that passes no subclass asks for the function as written and finds it.
    ///
    /// Returns the plain name when no specialization exists, which keeps a
    /// function past the specialization limit callable.
    pub(super) fn specialized_name(&self, name: &str, args: &[HirExprId]) -> String {
        let mut suffix = String::new();
        for (index, arg) in args.iter().enumerate() {
            let Type::Struct(id) = self.program.expr(*arg).type_of() else {
                continue;
            };
            if !self.classes.contains_key(&id) {
                continue;
            }
            suffix.push_str(&format!(
                "${index}${}",
                self.member_owner_name(Type::Struct(id))
            ));
        }
        let specialized = format!("{name}{suffix}");
        if !suffix.is_empty() && self.sig_index.contains_key(&specialized) {
            return specialized;
        }
        name.to_owned()
    }

    pub(super) fn analyze_user_call(
        &mut self,
        name: &str,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        self.analyze_user_call_hinted(name, args, span, None)
    }

    /// Type-checks a call whose arguments are analyzed.
    ///
    /// `chosen` is the declaration an earlier pass already resolved this call
    /// to. It is honored unless class specialization renamed the callee, in
    /// which case the specialized copy is a different function and is looked up
    /// as one.
    pub(in crate::typeck) fn analyze_user_call_hinted(
        &mut self,
        name: &str,
        args: &[HirExprId],
        span: kira_source::Span,
        chosen: Option<FuncId>,
    ) -> HirExprId {
        // A program may still declare a function called `sqrt`; a *call* of one
        // reaches the primitive only when nothing else answers to the name, so
        // the check runs after the user table below has been consulted.
        let specialized = self.specialized_name(name, args);
        let chosen = chosen.filter(|_| specialized == name);
        let name = &specialized;
        let candidates = self.visible_overloads(name);
        if candidates.is_empty() {
            if let Some(op) = kira_runtime_abi::MathOp::from_name(name) {
                return self.analyze_math_call(op, args, span);
            }
            if let Some(op) = super::math::wrapping_operator(name) {
                return self.analyze_wrapping_call(name, op, args, span);
            }
            if name == "scalarText" {
                return self.analyze_scalar_text_call(args, span);
            }
            self.emit(
                span,
                "KSEM061",
                format!("call to undefined function `{name}`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // The arguments are already analyzed here, so choosing among the
        // declarations of an overloaded name is a comparison rather than a
        // guess. A name declared once returns its one candidate untouched and
        // reports its own arity and type mistakes below, as it always did.
        let actual: Vec<Type> = args
            .iter()
            .map(|&arg| self.program.expr(arg).type_of())
            .collect();
        let id = match chosen
            .map(Ok)
            .unwrap_or_else(|| self.resolve_overload(&candidates, &actual))
        {
            Ok(id) => id,
            Err(OverloadFailure::Ambiguous(winners)) => {
                let list = self.overload_list(&winners);
                self.emit(
                    span,
                    "KSEM275",
                    format!("this call of `{name}` fits {list} equally well"),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            // Nothing fits. The first candidate carries the diagnostic, so an
            // overloaded name still says what it expected rather than only that
            // nothing matched.
            Err(OverloadFailure::None) => candidates[0],
        };
        if self.refuse_direct_async_call(id, span) {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let (params, ret) = {
            let (params, ret) = self.signature_of(id);
            (params.to_vec(), ret)
        };
        self.link_function(id, span);
        let mut args = args.to_vec();
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
            for (index, (arg, &expected)) in args.iter_mut().zip(params.iter()).enumerate() {
                let actual = self.program.expr(*arg).type_of();
                if !self.admits_argument(actual, expected) {
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
                // An `Any` parameter takes an erased value, and the erasure
                // belongs to the call site: the callee's body only ever sees the
                // boxed form.
                *arg = self.coerce_into(*arg, expected);
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(id),
            args,
            ty: ret,
            writebacks: Vec::new(),
        })
    }

    /// Analyzes `sqrt(x)` and the rest of the floating-point primitives.
    fn analyze_math_call(
        &mut self,
        op: kira_runtime_abi::MathOp,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let name = op.name();
        let expected = op.argument_count();
        if args.len() != expected {
            let arguments = if expected == 1 {
                "argument"
            } else {
                "arguments"
            };
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {expected} {arguments}, and this call passes {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // Every operand is checked before any is coerced, so a two-operand call
        // that is wrong in its second argument says so about that argument
        // rather than about the call.
        let mut operands = Vec::with_capacity(expected);
        for &arg in args {
            let actual = self.program.expr(arg).type_of();
            if !actual.assignable_to(Type::FLOAT) {
                self.emit(
                    span,
                    "KSEM063",
                    format!(
                        "`{name}` takes a `Float`, and this call passes a `{}`",
                        self.type_name(actual)
                    ),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            operands.push(self.coerce_into(arg, Type::FLOAT));
        }
        self.program
            .exprs
            .alloc(HirExpr::MathOperation { op, operands })
    }

    /// Analyzes `scalarText(codePoint)` — one Unicode scalar as text.
    fn analyze_scalar_text_call(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        let [value] = args else {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`scalarText` takes one argument, and this call passes {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let actual = self.program.expr(*value).type_of();
        if !actual.assignable_to(Type::INT) {
            self.emit(
                span,
                "KSEM063",
                format!(
                    "`scalarText` takes an `Int` code point, and this call passes a `{}`",
                    self.type_name(actual)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let value = self.coerce_into(*value, Type::INT);
        self.program.exprs.alloc(HirExpr::ScalarText { value })
    }
}
