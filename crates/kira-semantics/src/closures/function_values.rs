use kira_semantics_model::hir::CallableSignature;
use kira_semantics_model::hir::FieldOrder;
use super::*;

impl Analyzer<'_> {
    /// The type of `(params) -> result`, minting its representation struct on
    /// first mention.
    ///
    /// Idempotent by shape, so `(Int) -> Void` written in two files is one
    /// type and a closure made for one fits the other.
    ///
    /// The parameter *modes* are part of that shape. `(borrow Event) -> Void`
    /// and `(Event) -> Void` are two types, because a call through the first
    /// leaves the caller's value alone and a call through the second takes it —
    /// so a value of one is not a value of the other.
    pub(crate) fn function_type(
        &mut self,
        params: Vec<Type>,
        param_ownership: Vec<OwnershipMode>,
        result: Type,
    ) -> Type {
        // An unresolvable part makes the whole type unresolvable, exactly as it
        // does for an array element: interning `(<error>) -> Int` would mint a
        // row a second bad type would compare *equal* to.
        if params.contains(&Type::Error) || result == Type::Error {
            return Type::Error;
        }
        let key = (params.clone(), param_ownership.clone(), result);
        if let Some(id) = self.fn_types.lookup(&key) {
            return Type::Struct(id);
        }
        let name = self.function_type_name(&params, &param_ownership, result);
        // The name carries parentheses and an arrow, so it can collide with no
        // declared struct: an identifier holds neither.
        let Some(id) = self
            .program
            .types
            .structs_mut()
            .declare_function_type(StructDef {
                name,
                fields: vec![FieldDef {
                    name: "tag".to_owned(),
                    ty: Type::INT,
                    mutable: false,
                }],
                c_layout: false,
                drop_glue: None,
            })
        else {
            return Type::Error;
        };
        // Kept in step with the table: `struct_defaults` is indexed by the ids
        // the table mints, and this mints one outside `collect_structs`.
        self.struct_defaults.push(vec![None]);
        self.fn_types.insert(
            key,
            id,
            FnTypeInfo {
                params,
                param_ownership,
                result,
                dispatcher: None,
                impls: Vec::new(),
                named_functions: std::collections::HashMap::new(),
            },
        );
        Type::Struct(id)
    }

    /// The canonical spelling of a function type, which is also the name its
    /// representation struct is declared under — so every diagnostic that
    /// names a type names this one as it was written.
    ///
    /// The modes are spelled too, because they are part of the type: two
    /// function types that differ only in a mode must not print the same, or a
    /// mismatch between them reads as a type not matching itself.
    pub(crate) fn function_type_name(
        &self,
        params: &[Type],
        param_ownership: &[OwnershipMode],
        result: Type,
    ) -> String {
        let written: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(index, &ty)| {
                let name = self.type_name(ty);
                match param_ownership.get(index) {
                    Some(OwnershipMode::Owned) | None => name,
                    Some(mode) => format!("{} {name}", mode.spelling()),
                }
            })
            .collect();
        format!("({}) -> {}", written.join(", "), self.type_name(result))
    }

    /// Reserves the next temporary synthesized function id.
    ///
    /// Generic signatures may be discovered while a body is being analyzed, so
    /// synthesized functions cannot reserve positions in the final function
    /// vector yet. Reserving and filling are separate because a dispatcher's id
    /// is needed at the call site, long before its body can be built.
    pub(crate) fn reserve_synth(&mut self) -> FuncId {
        let id = FuncId(self.synth_base + self.synth.len() as u32);
        self.synth.push(None);
        id
    }

    /// How many synthesized ids have been reserved so far.
    ///
    /// What the fill passes watch to know whether building a body reserved
    /// another one.
    pub(crate) fn reserved_synth(&self) -> usize {
        self.synth.len()
    }

    /// Whether a reserved id is still waiting for its body.
    ///
    /// The fill passes run more than once, because a body may reserve an id
    /// while it is being built; asking this is what keeps the second run from
    /// analyzing an already-filled body again and reporting its diagnostics
    /// twice.
    pub(crate) fn synth_needs_body(&self, id: FuncId) -> bool {
        let index = (id.0 - self.synth_base) as usize;
        self.synth.get(index).is_some_and(Option::is_none)
    }

    /// Records a synthesized function's body against its reserved id.
    pub(crate) fn fill_synth(&mut self, id: FuncId, function: HirFunction) {
        let index = (id.0 - self.synth_base) as usize;
        if let Some(slot) = self.synth.get_mut(index) {
            *slot = Some(function);
        }
    }

    /// Type-checks a closure literal against the function type expected here.
    ///
    /// A closure never writes its parameter types, so without an expected type
    /// there is nothing to check the body against and nothing to build a value
    /// of — which is why an unexpected closure is an error rather than a
    /// guess.
    pub(crate) fn analyze_closure(
        &mut self,
        ctx: &mut FnCtx,
        params: &[ClosureParam],
        body: &Block,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let Some(repr) = expected.and_then(|ty| match ty {
            Type::Struct(id) if self.fn_types.get(id).is_some() => Some(id),
            _ => None,
        }) else {
            // An `Error` expectation already had its say.
            if expected != Some(Type::Error) {
                let found = match expected {
                    Some(ty) => format!("`{}` is expected here", self.type_name(ty)),
                    None => "nothing here says what its parameters are".to_owned(),
                };
                self.emit(
                    span,
                    "KSEM134",
                    format!("a closure needs a function type to check against, but {found}"),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let info = self.fn_types.get(repr).map(|info| {
            (
                info.params.clone(),
                info.param_ownership.clone(),
                info.result,
            )
        });
        let Some((param_types, modes, result)) = info else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        if params.len() != param_types.len() {
            self.emit(
                span,
                "KSEM135",
                format!(
                    "`{}` takes {} parameter(s), but this closure declares {}",
                    self.type_name(Type::Struct(repr)),
                    param_types.len(),
                    params.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let shape = ClosureShape {
            repr,
            params: &param_types,
            modes: &modes,
            result,
        };
        self.lift_closure(ctx, shape, params, body)
    }

    /// Resolves a bare function name as a first-class function value.
    ///
    /// A local or field of the same name has already won before this is called.
    /// With an expected function type, the named function must match it exactly;
    /// without one, its signature supplies the function value's type.
    pub(crate) fn analyze_named_function_reference(
        &mut self,
        name: &str,
        span: Span,
        expected: Option<Type>,
    ) -> Option<HirExprId> {
        let (target, params, result) = self
            .lookup_function(name)
            .map(|(id, params, result)| (id, params.to_vec(), result))?;
        // A named function's own parameter modes are part of the type it has,
        // so they take part in the match: `graphicsApplicationDefaultEvent`
        // declares `borrow`, and it fits a `(borrow GraphicsEvent) -> Void`
        // slot rather than an owned one.
        let modes = self.param_ownership(target);
        let repr = match expected {
            Some(Type::Struct(id)) if self.fn_types.get(id).is_some() => {
                let matches = self.fn_types.get(id).is_some_and(|info| {
                    info.params == params && info.param_ownership == modes && info.result == result
                });
                if !matches {
                    self.emit(
                        span,
                        "KSEM212",
                        format!(
                            "function `{name}` has type `{}`, which does not match expected `{}`",
                            self.function_type_name(&params, &modes, result),
                            self.type_name(Type::Struct(id))
                        ),
                    );
                    return Some(self.program.exprs.alloc(HirExpr::Error));
                }
                id
            }
            Some(Type::Error) => return Some(self.program.exprs.alloc(HirExpr::Error)),
            Some(expected) => {
                self.emit(
                    span,
                    "KSEM212",
                    format!(
                        "function `{name}` is a function value, but `{}` is expected here",
                        self.type_name(expected)
                    ),
                );
                return Some(self.program.exprs.alloc(HirExpr::Error));
            }
            None => match self.function_type(params.clone(), modes.clone(), result) {
                Type::Struct(id) => id,
                _ => return Some(self.program.exprs.alloc(HirExpr::Error)),
            },
        };
        self.link_function(target, span);
        let shape = ClosureShape {
            repr,
            params: &params,
            modes: &modes,
            result,
        };
        Some(self.named_function_value(shape, target, name))
    }

    /// Builds or reuses one named function's tag and returns its function value.
    fn named_function_value(
        &mut self,
        shape: ClosureShape<'_>,
        target: FuncId,
        name: &str,
    ) -> HirExprId {
        let repr = shape.repr;
        let params = shape.params;
        let modes = shape.modes;
        let result = shape.result;
        let tag = match self
            .fn_types
            .get(repr)
            .and_then(|info| info.named_functions.get(&target))
            .copied()
        {
            Some(tag) => tag,
            None => {
                let display_tag = self
                    .fn_types
                    .get(repr)
                    .map_or(0, |info| info.impls.len() as u32);
                let candidate = self
                    .function_identity(target)
                    .map(|(source, span, identity)| stable_impl_tag(source, span, identity))
                    .unwrap_or_else(|| {
                        stable_impl_tag(crate::FILE_SOURCE_ID, Span::new(0, target.0), name)
                    });
                let tag = self
                    .fn_types
                    .get(repr)
                    .map_or(candidate, |info| unique_impl_tag(info, candidate));
                let shape = ClosureShape {
                    repr,
                    params,
                    modes,
                    result,
                };
                let wrapper = self.named_function_wrapper(shape, target, name, display_tag);
                if let Some(info) = self.fn_types.get_mut(repr) {
                    info.impls.push(ClosureImpl {
                        tag,
                        function: wrapper,
                    });
                    info.named_functions.insert(target, tag);
                }
                tag
            }
        };
        let tag_expr = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        let expr = self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: repr,
            fields: vec![tag_expr],
            order: FieldOrder::Declared,
        });
        self.closure_sites.push(ClosureSite {
            expr,
            repr,
            tag,
            capture_fields: Vec::new(),
            capture_values: Vec::new(),
        });
        expr
    }

    /// Synthesizes the environment-taking adapter the closure dispatcher calls.
    ///
    /// The adapter drops the environment and forwards the rest, so a `borrow
    /// mut` parameter shifts by one on the way through: the wrapper's slot
    /// `index + 1` is the target's slot `index`, and the writeback names the
    /// target's. Without it the wrapper would call a by-reference function and
    /// throw away what it wrote, which is what the bytecode compiler's
    /// missing-writeback invariant catches.
    fn named_function_wrapper(
        &mut self,
        shape: ClosureShape<'_>,
        target: FuncId,
        name: &str,
        tag: u32,
    ) -> FuncId {
        let repr = shape.repr;
        let params = shape.params;
        let modes = shape.modes;
        let result = shape.result;
        let wrapper = self.reserve_synth();
        let mut ctx = FnCtx::new(result);
        ctx.declare_hidden(Type::Struct(repr), false);
        let mut args = Vec::with_capacity(params.len());
        let mut writebacks = Vec::new();
        for (index, &ty) in params.iter().enumerate() {
            let mode = modes.get(index).copied().unwrap_or(OwnershipMode::Owned);
            let mutable = mode == OwnershipMode::BorrowMut;
            let local = ctx.declare_hidden_as(ty, mutable, mode);
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
            if mutable {
                writebacks.push(kira_semantics_model::hir::HirWriteback {
                    param: index as u32,
                    place: kira_semantics_model::hir::HirPlace {
                        local,
                        path: Vec::new(),
                    },
                });
            }
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: kira_semantics_model::hir::Callee::User(target),
            args,
            ty: result,
            writebacks,
        });
        let body = if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        };
        self.fill_synth(
            wrapper,
            HirFunction {
                name: format!("{name}$reference#{tag}"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: ctx.locals,
                body,
                is_main: false,
                is_main_thread: false,
                is_async: false,
                // The engine the reference was written on, exactly as a closure
                // literal takes it in `lift_closure`. A wrapper is not a function
                // the author wrote — it exists only to carry one — so leaving it
                // `Inherited` sent it to the VM regardless of where it was taken,
                // and a wrapper for a `borrow mut` parameter is a writeback call,
                // which the bytecode compiler refuses when its callee is native.
                execution: self.current_execution,
                mutates_self: false,
                name_span: Span::new(0, 0),
                signature: CallableSignature::synthesized(&[], result),
            },
        );
        wrapper
    }
}
