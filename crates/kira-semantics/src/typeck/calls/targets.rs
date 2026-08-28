use super::*;

use crate::typeck::overloads::OverloadFailure;

impl Analyzer<'_> {
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
        type_args: &[kira_syntax_model::ast::TypeRefId],
        args: &[CallArg],
        trailing: &[HirExprId],
    ) -> CallTarget {
        let candidates = self.visible_overloads(name);
        if candidates.is_empty() && self.is_generic_function(name) {
            return match self
                .instantiate_generic_function(ctx, name, type_args, leading, args, trailing)
            {
                Some(id) => CallTarget::Chosen(id, self.param_types(id)),
                None => CallTarget::Invalid,
            };
        }
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
    fn specialized_name(&self, name: &str, args: &[HirExprId]) -> String {
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
                self.program.types.type_name(Type::Struct(id))
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
    pub(super) fn analyze_user_call_hinted(
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
}
