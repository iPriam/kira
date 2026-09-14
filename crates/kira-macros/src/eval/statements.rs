use super::*;

impl Evaluator<'_> {
    /// Runs a block in its own scope.
    pub(super) fn block(&mut self, block: &Block) -> Result<Flow, EvalError> {
        self.scopes.push(HashMap::new());
        let mut flow = Flow::Normal;
        for &id in &block.stmts {
            flow = self.statement(id)?;
            if !matches!(flow, Flow::Normal) {
                break;
            }
        }
        self.scopes.pop();
        Ok(flow)
    }

    fn statement(&mut self, id: StmtId) -> Result<Flow, EvalError> {
        self.charge()?;
        match self.stmt(id).clone() {
            Stmt::Let { name, init, .. } => {
                let value = self.value(init)?;
                let name = self.name(name).to_owned();
                self.bind(&name, value);
                Ok(Flow::Normal)
            }
            Stmt::Assign {
                target, op, value, ..
            } => {
                let evaluated = self.value(value)?;
                match self.expr(target).clone() {
                    Expr::Name { symbol, .. } => {
                        let name = self.name(symbol).to_owned();
                        // A compound assignment folds the current value in
                        // before storing: `x += y` stores `x + y`.
                        let evaluated = match op {
                            Some(op) => {
                                let current = self.value(target)?;
                                methods::binary(op, current, evaluated)?
                            }
                            None => evaluated,
                        };
                        self.assign(&name, evaluated)?;
                        Ok(Flow::Normal)
                    }
                    other => Err(EvalError::unsupported(format!(
                        "assigning to a {}",
                        shape(&other)
                    ))),
                }
            }
            Stmt::Return { value, .. } => {
                let returned = match value {
                    Some(id) => self.value(id)?,
                    None => Value::Void,
                };
                Ok(Flow::Return(returned))
            }
            Stmt::Expr { expr, .. } => {
                self.value(expr)?;
                Ok(Flow::Normal)
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let taken = self.condition(cond)?;
                if taken {
                    self.block(&then_block)
                } else if let Some(otherwise) = else_block {
                    self.block(&otherwise)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::While { cond, body, .. } => {
                let mut rounds = 0u32;
                while self.condition(cond)? {
                    rounds += 1;
                    if rounds > LOOP_LIMIT {
                        return Err(EvalError::coded(
                            diagnostics::DEPTH_LIMIT,
                            format!(
                                "a `while` in an `expand` body ran more than {LOOP_LIMIT} times"
                            ),
                        ));
                    }
                    match self.block(&body)? {
                        Flow::Return(value) => return Ok(Flow::Return(value)),
                        Flow::Break => break,
                        Flow::Normal | Flow::Continue => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::For {
                name,
                iterable,
                body,
                ..
            } => self.for_loop(name, &iterable, &body),
            Stmt::Match { subject, arms, .. } => self.match_statement(subject, &arms),
            Stmt::Attempt { body, handlers, .. } => self.attempt_statement(&body, &handlers),
            Stmt::Break { .. } => Ok(Flow::Break),
            Stmt::Continue { .. } => Ok(Flow::Continue),
            other => Err(EvalError::unsupported(statement_shape(&other))),
        }
    }

    /// Runs an `attempt { … } handle { … }`.
    ///
    /// The body runs statement by statement until a `try` unwraps a case that
    /// turned out to be the failure one, at which point the rest of the body is
    /// skipped and the arm naming that failure runs instead — which is what the
    /// language does, and the reason statements after a `try` nest into its
    /// success branch there.
    ///
    /// `Result`-shaped is structural here as it is everywhere else: any case
    /// named `Ok` succeeds and carries the value on, any other case is the
    /// failure and is routed. Nothing nominal is required, so a body may `try`
    /// an enum it declared itself.
    fn attempt_statement(
        &mut self,
        body: &Block,
        handlers: &[MatchArm],
    ) -> Result<Flow, EvalError> {
        self.scopes.push(HashMap::new());
        let mut failure = None;
        let mut flow = Flow::Normal;
        for &id in &body.stmts {
            match self.try_statement(id)? {
                Attempted::Ran(next) => {
                    flow = next;
                    if !matches!(flow, Flow::Normal) {
                        break;
                    }
                }
                Attempted::Failed(case) => {
                    failure = Some(case);
                    break;
                }
            }
        }
        self.scopes.pop();
        let Some(case) = failure else {
            return Ok(flow);
        };
        let variant = case.variant.clone();
        let subject = Value::EnumCase(Box::new(case));
        if let Some(flow) = self.run_arms(handlers, &subject)? {
            return Ok(flow);
        }
        Err(EvalError::unsupported(format!(
            "an `attempt` with no handler for `{variant}`"
        )))
    }

    /// Runs one statement of an `attempt` body, reporting a `try` that failed.
    fn try_statement(&mut self, id: StmtId) -> Result<Attempted, EvalError> {
        let Stmt::Let { name, init, .. } = self.stmt(id).clone() else {
            return Ok(Attempted::Ran(self.statement(id)?));
        };
        let Expr::Try { value, .. } = self.expr(init).clone() else {
            return Ok(Attempted::Ran(self.statement(id)?));
        };
        let outcome = self.value(value)?;
        let Value::EnumCase(case) = outcome else {
            return Err(EvalError::unsupported(format!(
                "`try` on a `{}`; it unwraps a `Result`-shaped enum case",
                outcome.type_name()
            )));
        };
        if case.variant != "Ok" {
            return Ok(Attempted::Failed(*case));
        }
        let name = self.name(name).to_owned();
        self.bind(&name, case.payload.clone().unwrap_or(Value::Void));
        Ok(Attempted::Ran(Flow::Normal))
    }

    /// Runs a `match` over an enum case.
    ///
    /// An arm selects by variant name, which is all a case carries that matters
    /// here: a bare `.Variant` never knew its enum, so matching on the name is
    /// the only rule that works for both a case read from reflection and one the
    /// body wrote itself.
    ///
    /// A subject no arm names is an error rather than a fall-through. The
    /// language checks exhaustiveness before a program runs; an `expand` body is
    /// evaluated rather than compiled, so the equivalent guarantee has to be
    /// this — a macro that forgot a variant hears about it instead of silently
    /// producing nothing.
    fn match_statement(&mut self, subject: ExprId, arms: &[MatchArm]) -> Result<Flow, EvalError> {
        let value = self.value(subject)?;
        let subject_label = match &value {
            Value::EnumCase(case) => case.variant.clone(),
            other => other.type_name().to_owned(),
        };
        if let Some(flow) = self.run_arms(arms, &value)? {
            return Ok(flow);
        }
        Err(EvalError::unsupported(format!(
            "a `match` with no arm for `{subject_label}`"
        )))
    }

    /// Runs the first arm whose head matches `subject`, returning its flow, or
    /// `None` when no arm matched.
    fn run_arms(&mut self, arms: &[MatchArm], subject: &Value) -> Result<Option<Flow>, EvalError> {
        for arm in arms {
            let mut matched = None;
            for pattern in &arm.patterns {
                if let Some(binding) = self.pattern_matches(pattern, subject)? {
                    matched = Some(binding);
                    break;
                }
            }
            let Some(binding) = matched else {
                continue;
            };
            self.scopes.push(HashMap::new());
            if let Some((name, payload)) = binding {
                self.bind(&name, payload);
            }
            let mut flow = Flow::Normal;
            for &id in &arm.body.stmts {
                flow = self.statement(id)?;
                if !matches!(flow, Flow::Normal) {
                    break;
                }
            }
            self.scopes.pop();
            return Ok(Some(flow));
        }
        Ok(None)
    }

    /// Whether one pattern matches `subject`. The outer `Option` is the match;
    /// the inner is the payload binding a matching variant arm establishes.
    fn pattern_matches(
        &mut self,
        pattern: &MatchPattern,
        subject: &Value,
    ) -> Result<Option<Option<(String, Value)>>, EvalError> {
        match pattern {
            MatchPattern::Wildcard { .. } => Ok(Some(None)),
            MatchPattern::Variant {
                variant, binding, ..
            } => {
                let Value::EnumCase(case) = subject else {
                    return Ok(None);
                };
                if self.name(*variant) != case.variant {
                    return Ok(None);
                }
                let bind = binding.map(|binding| {
                    (
                        self.name(binding.name).to_owned(),
                        case.payload.clone().unwrap_or(Value::Void),
                    )
                });
                Ok(Some(bind))
            }
            MatchPattern::Value { expr, .. } => {
                let literal = self.value(*expr)?;
                let equal = methods::binary(BinaryOp::Eq, literal, subject.clone())?;
                Ok(matches!(equal, Value::Bool(true)).then_some(None))
            }
            MatchPattern::Range { start, end, .. } => {
                let low = self.value(*start)?;
                let high = self.value(*end)?;
                match (low, subject, high) {
                    (Value::Int(low), Value::Int(subject), Value::Int(high)) => {
                        Ok((*subject >= low && *subject < high).then_some(None))
                    }
                    _ => Ok(None),
                }
            }
        }
    }

    fn for_loop(
        &mut self,
        name: kira_core::Symbol,
        iterable: &ForIterable,
        body: &Block,
    ) -> Result<Flow, EvalError> {
        let name = self.name(name).to_owned();
        let items = match iterable {
            ForIterable::Range { start, end } => {
                let (Value::Int(from), Value::Int(to)) = (self.value(*start)?, self.value(*end)?)
                else {
                    return Err(EvalError::unsupported("a range over non-integers"));
                };
                // Materialized lazily and bounded like a `while`: a range the
                // width of an address space would otherwise be collected up
                // front and end the compiler with an allocation failure
                // instead of this diagnostic.
                let count = to.saturating_sub(from).max(0);
                if count > i64::from(LOOP_LIMIT) {
                    return Err(EvalError::coded(
                        diagnostics::DEPTH_LIMIT,
                        format!(
                            "a `for` in an `expand` body ranges over more than {LOOP_LIMIT} items"
                        ),
                    ));
                }
                (from..to).map(Value::Int).collect()
            }
            ForIterable::Each { array } => match self.value(*array)? {
                Value::Array(items) => items,
                other => {
                    return Err(EvalError::unsupported(format!(
                        "iterating a `{}`",
                        other.type_name()
                    )));
                }
            },
        };
        for item in items {
            self.scopes.push(HashMap::new());
            self.bind(&name, item);
            let flow = self.block(body);
            self.scopes.pop();
            match flow? {
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Break => break,
                Flow::Normal | Flow::Continue => {}
            }
        }
        Ok(Flow::Normal)
    }

    pub(super) fn condition(&mut self, id: ExprId) -> Result<bool, EvalError> {
        let value = self.value(id)?;
        value.as_bool().ok_or_else(|| {
            EvalError::unsupported(format!("a `{}` where a Bool is needed", value.type_name()))
        })
    }
}
