//! Checking `@Main` and analyzing one function body.
//!
//! `check_main` is here rather than beside the driver because it is the same
//! kind of work as analyzing any other function — it just has one extra rule.

use super::*;

impl<'a> Analyzer<'a> {
    /// Checks the entrypoint rule for the kind of thing being built.
    ///
    /// An application needs exactly one `@Main`; a library must have none. Both
    /// halves are decided here rather than in a backend because the answer is
    /// the same for every backend: an entrypoint is a property of the program,
    /// not of the engine that runs it.
    pub(super) fn check_main(&mut self) {
        // Snapshot the entrypoint's identity before emitting, so the
        // immutable borrow of `self.sigs` does not overlap `self.emit`.
        let main = self.sigs.iter().position(|sig| sig.is_main).map(|index| {
            let sig = &self.sigs[index];
            (FuncId(index as u32), sig.params.is_empty(), sig.name_span)
        });
        match (self.build_kind, main) {
            (BuildKind::Application, None) => {
                self.emit(
                    Span::new(0, 0),
                    "KSEM011",
                    "program has no `@Main` function to run",
                );
            }
            (BuildKind::Application, Some((id, no_params, name_span))) => {
                if !no_params {
                    self.emit(name_span, "KSEM012", "`@Main` must take no parameters");
                }
                self.program.main = Some(id);
            }
            // A library has no entrypoint by definition, so its absence is not
            // an error and `program.main` stays `None`. A test run is the same
            // about absence, and unlike a library it accepts one when written.
            (BuildKind::Library | BuildKind::Test, None) => {}
            (BuildKind::Test, Some((id, no_params, name_span))) => {
                if !no_params {
                    self.emit(name_span, "KSEM012", "`@Main` must take no parameters");
                }
                self.program.main = Some(id);
            }
            (BuildKind::Library, Some((_, _, name_span))) => {
                self.emit(
                    name_span,
                    "KSEM255",
                    "a library package cannot declare `@Main`: a library is \
                     entered by its consumer, not run",
                );
            }
        }
    }

    pub(super) fn analyze_function(&mut self, id: FuncId, callable: &Callable<'a>) -> HirFunction {
        let function = callable.function;
        // The body resolves qualified names against the imports of the file it
        // was written in — not the entry file's, and not the union of all of
        // them. That is what "file-scoped" means.
        self.source = callable.source;
        let outer_bindings =
            std::mem::replace(&mut self.type_bindings, callable.type_bindings.clone());
        let sig_return = self.sigs[id.0 as usize].return_type;
        self.current_execution = function.execution;
        let mut ctx = FnCtx::new(sig_return);
        // Which names this body's closures mention, decided from the syntax
        // before anything is analyzed: a `var` among them is boxed where it is
        // declared, which is earlier than the capture that needs the box is
        // discovered. See `crate::closures::captures`.
        ctx.set_closure_mentions(std::rc::Rc::new(
            crate::closures::captures::names_closures_mention(
                self.tree,
                self.interner,
                &function.body,
            ),
        ));
        // A method's receiver is local 0, named `self`. A non-mutating method
        // receives it as an immutable copy — writing to it would change nothing
        // the caller could see. A mutating method receives it as a mutable,
        // owned value that the call site writes back afterwards, so `self.field
        // = x` is a real write to the caller's storage rather than a lost one.
        if let Some(owner) = callable.receiver {
            let mutates = self.mutates_self(id);
            let (mutable, mode) = if mutates {
                (true, OwnershipMode::Owned)
            } else {
                (false, OwnershipMode::BorrowRead)
            };
            ctx.declare_param("self", owner, mutable, mode);
            ctx.receiver = match owner {
                Type::Struct(id) => Some(id),
                _ => None,
            };
        }
        // Parameters become the next locals, each carrying the mode its
        // declaration asked for. Reading the mode off the signature rather
        // than off the syntax again keeps the `borrow mut` refusal from being
        // reported a second time here.
        let param_modes = self.sigs[id.0 as usize].param_ownership.clone();
        let param_types = self.sigs[id.0 as usize].params.clone();
        let receiver_slots = usize::from(callable.receiver.is_some());
        for (index, param) in function.params.iter().enumerate() {
            // The type comes off the signature for the same reason the mode
            // does, and for one more: a specialized copy takes a subclass where
            // the syntax wrote the parent, and re-resolving the syntax here
            // would type the body's parameter as the parent again — which is
            // exactly the override-not-taken bug specialization exists to fix.
            let ty = param_types
                .get(index + receiver_slots)
                .copied()
                .unwrap_or_else(|| self.resolve_type_ref(param.ty));
            let name = self.interner.resolve(param.name).to_owned();
            let mode = param_modes
                .get(index + receiver_slots)
                .copied()
                .unwrap_or(OwnershipMode::Owned);
            // A `borrow mut` parameter is the only kind a body may write
            // through; every other parameter is an immutable binding.
            let mutable = mode == OwnershipMode::BorrowMut;
            let local = ctx.declare_param(&name, ty, mutable, mode);
            ctx.note_binding_span(local, param.name_span);
        }
        let param_count = function.params.len() as u32 + u32::from(callable.receiver.is_some());
        let body = self.analyze_block(&mut ctx, &function.body);
        self.check_native_state_handles(&ctx);
        // Every expression this body could build now exists, so a member read
        // still unclaimed is one no borrowed position took.
        self.report_drop_extractions();
        // Definite-return check: a non-Void function must return on every
        // control path (the reference rejects this too). `Error` returns are
        // skipped to avoid cascading on an already-broken signature.
        if sig_return != Type::Void
            && sig_return != Type::Error
            && !self.body_definitely_returns(&body)
        {
            let name = self.callable_name(callable);
            self.emit(
                function.name_span,
                "KSEM033",
                format!("`{name}` may finish without returning a value"),
            );
        }
        let result = HirFunction {
            // The symbol, not the written name: two overloads share a name and
            // a backend has one function per symbol.
            name: self.function_symbol(id),
            param_count,
            return_type: sig_return,
            locals: ctx.locals,
            body,
            is_main: function.is_main,
            is_async: function.is_async,
            execution: function.execution,
            mutates_self: self.mutates_self(id),
            name_span: function.name_span,
        };
        self.type_bindings = outer_bindings;
        result
    }
}
