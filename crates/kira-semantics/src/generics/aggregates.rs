//! Generic struct and class instantiation.

use super::*;

impl<'a> Analyzer<'a> {
    /// Resolves a written `Name<Args>` to the enum its instantiation declares.
    pub(crate) fn resolve_generic_instantiation(
        &mut self,
        name: kira_core::Symbol,
        name_span: Span,
        args: &[TypeRefId],
        span: Span,
        context: &NameContext,
    ) -> Type {
        let written = self.interner.resolve(name).to_owned();
        // A generic template is keyed by its bare name, so the qualifier here
        // buys only the file-scope check the split performs.
        let Some(text) = self
            .split_module_qualifier(&written, name_span)
            .map(|qualified| qualified.text)
        else {
            // Reported already; still resolve the arguments so their own
            // mistakes are not hidden behind one unimported module.
            for &arg in args {
                self.resolve_type_in(arg, context);
            }
            return Type::Error;
        };
        // Arguments resolve in the *use site's* scope, which is the current
        // binding frame — that is what makes `Result<Value, E>` inside another
        // template's body mean what it says.
        let mut resolved: Vec<Type> = Vec::with_capacity(args.len());
        let mut any_error = false;
        for &arg in args {
            let ty = self.resolve_type_in(arg, context);
            any_error |= ty == Type::Error;
            resolved.push(ty);
        }
        let enum_template = self.generic_enums.get(&text).copied();
        let aggregate_template = self.generic_aggregates.get(&text).copied();
        let trait_template = self
            .traits
            .get(&text)
            .filter(|trait_info| !trait_info.type_params.is_empty())
            .cloned();
        let Some(arity) = enum_template
            .map(|template| template.decl.type_params.len())
            .or_else(|| aggregate_template.map(|template| template.type_params().len()))
            .or_else(|| {
                trait_template
                    .as_ref()
                    .map(|template| template.type_params.len())
            })
        else {
            self.report_not_generic(&text, name_span, span);
            return Type::Error;
        };
        if resolved.len() != arity {
            self.emit(
                span,
                "KSEM174",
                format!(
                    "generic enum `{text}` takes {arity} type argument{}, but {} {} written",
                    if arity == 1 { "" } else { "s" },
                    resolved.len(),
                    if resolved.len() == 1 { "was" } else { "were" }
                ),
            );
            return Type::Error;
        }
        // An argument that did not resolve would mint a row named
        // `Result<<error>, E>` that a second unresolved name would compare
        // *equal* to — turning one mistake into two unrelated types that
        // type-check against each other. Same reasoning as `[<error>]`.
        if any_error {
            return Type::Error;
        }
        if let Some(template) = enum_template {
            self.instantiate(&text, template, &resolved, span)
        } else if let Some(template) = aggregate_template {
            self.instantiate_aggregate(&text, template, &resolved, span)
        } else {
            let template = trait_template.expect("trait template selected above");
            let key = self.mangle(&text, &resolved);
            if !self.traits.contains_key(&key) {
                self.instantiate_trait(&text, &key, &template, &resolved, span);
            }
            self.reserve_trait_existential(&key, span)
                .map_or(Type::Error, Type::Enum)
        }
    }

    /// Mints the ordinary struct row for a generic aggregate. A class has the
    /// same storage as a struct, so its generic parents are resolved and
    /// flattened through the same class layout machinery as an ordinary class.
    fn instantiate_aggregate(
        &mut self,
        text: &str,
        template: GenericAggregate<'a>,
        args: &[Type],
        span: Span,
    ) -> Type {
        let mangled = self.mangle(text, args);
        let owner = self
            .imports
            .package_of(template.source())
            .map(str::to_owned);
        if let Some(id) = self
            .program
            .types
            .structs()
            .lookup_owned(owner.as_deref(), &mangled)
        {
            return Type::Struct(id);
        }
        if self.instantiation_depth >= MAX_INSTANTIATION_DEPTH {
            self.emit(
                span,
                "KSEM175",
                format!("generic {text} instantiates itself without end (gave up at `{mangled}`)"),
            );
            return Type::Error;
        }
        let params = template.type_params();
        if params.iter().any(|param| !param.bounds.is_empty()) {
            let fresh = !self.pending_bounds.iter().any(|entry| entry.key == mangled);
            if fresh {
                self.pending_bounds.push(PendingBoundCheck {
                    key: mangled.clone(),
                    template: text.to_owned(),
                    args: args.to_vec(),
                    source: self.source,
                    declaration_source: template.source(),
                    span,
                    kind: match template {
                        GenericAggregate::Struct { .. } => "struct",
                        GenericAggregate::Class { .. } => "class",
                    },
                });
            }
        }
        let bindings: TypeBindings = params
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .zip(args.iter().copied())
            .collect();
        let outer_bindings = std::mem::replace(&mut self.type_bindings, bindings.clone());
        let outer_source = self.source;
        self.source = template.source();
        self.instantiation_depth += 1;
        let class_parents = match template {
            GenericAggregate::Class { decl, .. } => self.resolve_parents(decl),
            GenericAggregate::Struct { .. } => Vec::new(),
        };
        let (mut def, defaults, methods, is_class, class_slots) = match template {
            GenericAggregate::Struct { decl, .. } => {
                let (def, defaults) = self.resolve_struct_def(decl);
                let methods: Vec<OwnMethod> = decl
                    .methods
                    .iter()
                    .map(|method| OwnMethod {
                        key: self.member_key(self.interner.resolve(method.name), &method.params),
                    })
                    .collect();
                (def, defaults, methods, false, None)
            }
            GenericAggregate::Class { decl, .. } => {
                let flat = self.flatten_fields(decl, &mangled, &class_parents);
                let class_slots: Vec<(Option<StructId>, String)> = flat
                    .iter()
                    .map(|field| (field.owner, field.plain.clone()))
                    .collect();
                let fields: Vec<FieldDef> = flat
                    .iter()
                    .map(|field| FieldDef {
                        name: field.storage.clone(),
                        ty: field.ty,
                        mutable: field.mutable,
                    })
                    .collect();
                let defaults: Vec<Option<FieldDefault>> =
                    flat.iter().map(|field| field.default).collect();
                let methods: Vec<OwnMethod> = decl
                    .methods
                    .iter()
                    .map(|method| OwnMethod {
                        key: self.member_key(
                            self.interner.resolve(method.function.name),
                            &method.function.params,
                        ),
                    })
                    .collect();
                (
                    StructDef {
                        name: mangled.clone(),
                        fields,
                        c_layout: false,
                        drop_glue: None,
                    },
                    defaults,
                    methods,
                    true,
                    Some(class_slots),
                )
            }
        };
        // `resolve_struct_def` preserves the source declaration's bare name,
        // but this row is the concrete nominal type. The mangled name is the
        // identity used by lookup, constructor resolution, method qualification,
        // and backend symbol generation; leaving the template name here makes
        // `Box<Int>` collide with every other specialization and leaves its
        // fields unreachable from the concrete row.
        def.name = mangled.clone();
        self.instantiation_depth -= 1;
        self.source = outer_source;
        self.type_bindings = outer_bindings;
        let Some(id) = self
            .program
            .types
            .structs_mut()
            .declare_owned(owner.as_deref(), def)
        else {
            return Type::Error;
        };
        self.struct_defaults.push(defaults);
        self.struct_sources.insert(id, template.source());
        self.generic_instance_templates.insert(id, text.to_owned());
        self.generic_instance_arguments.insert(
            id,
            Instantiation {
                template: text.to_owned(),
                arguments: args.to_vec(),
            },
        );
        self.own_methods.insert(id, methods.clone());
        if is_class {
            let mut info = ClassInfo {
                ancestors: class_parents
                    .iter()
                    .flat_map(|parent| self.ancestors_of(*parent))
                    .collect(),
                ..ClassInfo::default()
            };
            for (index, (owner, plain)) in class_slots
                .expect("a class specialization carries its flattened slots")
                .into_iter()
                .enumerate()
            {
                let owner = owner.unwrap_or(id);
                info.qualified_fields
                    .insert((owner, plain.clone()), index as u32);
                info.slot_origin.push((owner, plain));
                if self
                    .struct_defaults
                    .get(id.index() as usize)
                    .and_then(|row| row.get(index))
                    .and_then(Option::as_ref)
                    .is_none()
                {
                    info.required_slots.push(index as u32);
                }
            }
            self.record_bare_fields(&mut info);
            self.resolve_methods(id, &class_parents, &mut info);
            self.classes.insert(id, info);
            if let GenericAggregate::Class { decl, .. } = template {
                self.check_overrides(decl, id);
            }
        }
        let function_bindings = bindings;
        if is_class {
            let methods: Vec<(StructId, String)> = self
                .classes
                .get(&id)
                .map(|info| info.qualified_methods.iter().cloned().collect())
                .unwrap_or_default();
            for (owner, key) in methods {
                let Some((function, source)) = self.method_ast(owner, &key) else {
                    continue;
                };
                let callable = Callable {
                    receiver: Some(id),
                    origin: Some(owner),
                    specialize: Vec::new(),
                    initializes: None,
                    function,
                    source,
                    type_bindings: self.generic_method_bindings(owner),
                };
                self.register_generic_method_callable(callable);
            }
        } else if let GenericAggregate::Struct { decl, source } = template {
            for method in &decl.methods {
                let callable = Callable {
                    receiver: Some(id),
                    origin: None,
                    specialize: Vec::new(),
                    initializes: None,
                    function: method,
                    source,
                    type_bindings: function_bindings.clone(),
                };
                self.register_generic_method_callable(callable);
            }
        }
        self.register_generic_aggregate_conformances(id, template, &function_bindings);
        Type::Struct(id)
    }

    /// The substitution belonging to a method's generic owner. An inherited
    /// method body keeps the parent's arguments, while a child receiver gets
    /// the child's own specialization; using the receiver's bindings for both
    /// would silently resolve the parent's `Value` against the wrong type.
    pub(crate) fn generic_method_bindings(&self, owner: StructId) -> TypeBindings {
        let Some(template_name) = self.generic_instance_templates.get(&owner) else {
            return Vec::new();
        };
        let Some(template) = self.generic_aggregates.get(template_name) else {
            return Vec::new();
        };
        let Some(arguments) = self.generic_instance_arguments.get(&owner) else {
            return Vec::new();
        };
        template
            .type_params()
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .zip(arguments.arguments.iter().copied())
            .collect()
    }

    /// Records one method of a concrete aggregate specialization. Before the
    /// initial callable pass the row is kept beside the other declarations; a
    /// specialization discovered while a body is analyzed must instead reserve
    /// its signature now and join the late body queue. Both paths use the same
    /// callable, so a method has one id and one body on every backend.
    fn register_generic_method_callable(&mut self, callable: Callable<'a>) {
        if self.generic_signatures_open {
            let id = kira_semantics_model::hir::FuncId(self.sigs.len() as u32);
            self.collect_signatures(std::slice::from_ref(&callable));
            self.generic_callables.push((id, callable));
        } else {
            self.generic_method_callables.push(callable);
        }
    }

    /// Activates the conformance list written on one generic aggregate for its
    /// newly minted concrete row. A template has no `StructId` during the
    /// normal conformance pass, so this is the point where the declaration's
    /// intrinsic promise becomes visible to bounds and trait-member lookup.
    fn register_generic_aggregate_conformances(
        &mut self,
        id: StructId,
        template: GenericAggregate<'a>,
        bindings: &TypeBindings,
    ) {
        let (declared, source, provided) = match template {
            GenericAggregate::Struct { decl, source } => (
                decl.traits.as_slice(),
                source,
                decl.methods
                    .iter()
                    .map(|method| self.interner.resolve(method.name).to_owned())
                    .collect::<HashSet<_>>(),
            ),
            GenericAggregate::Class { decl, source } => (
                decl.traits.as_slice(),
                source,
                decl.methods
                    .iter()
                    .map(|method| self.interner.resolve(method.function.name).to_owned())
                    .collect::<HashSet<_>>(),
            ),
        };
        if declared.is_empty() {
            return;
        }
        let outer_bindings = std::mem::replace(&mut self.type_bindings, bindings.clone());
        let outer_source = self.source;
        self.source = source;
        let type_name = self.program.types.type_name(Type::Struct(id));
        for entry in declared {
            let written_name = self.interner.resolve(entry.name).to_owned();
            let Some(trait_name) = self.resolve_trait_ref(entry) else {
                if !is_builtin_trait(&written_name) && !self.traits.contains_key(&written_name) {
                    self.source = source;
                    self.emit(
                        entry.span,
                        "KSEM289",
                        format!(
                            "`{written_name}` is not a trait, so `{type_name}` cannot conform to it"
                        ),
                    );
                }
                continue;
            };
            if !self.conformance_is_coherent(&trait_name, id, source) {
                self.source = source;
                self.emit(
                    entry.span,
                    "KSEM291",
                    format!(
                        "`{type_name}` may conform to `{trait_name}` only where one of them is \
                         declared"
                    ),
                );
                continue;
            }
            if self.conforms_to(id, &trait_name) {
                continue;
            }
            self.conformances.push(crate::traits::Conformance {
                contract: crate::traits::Contract::Trait(trait_name.clone()),
                ty: id,
                source,
                span: entry.span,
                via_family: None,
                provided: provided.clone(),
            });
            if self.generic_signatures_open {
                self.register_late_trait_defaults(id, &trait_name, &provided);
            }
        }
        self.source = outer_source;
        self.type_bindings = outer_bindings;
        // The initial pass has already checked the rows that existed before
        // this instantiation. Recheck only the suffix, including any defaults
        // just registered above, so a bad generic claim is diagnosed at the
        // concrete use while ordinary claims remain exactly once.
        if self.generic_signatures_open {
            self.check_conformances();
        }
    }

    /// Registers default members for a conformance discovered after the
    /// ordinary callable list was sealed. The body is queued with the same
    /// concrete receiver as a normal trait default, making a late generic row
    /// indistinguishable from one that was known during collection.
    fn register_late_trait_defaults(
        &mut self,
        receiver: StructId,
        trait_name: &str,
        provided: &HashSet<String>,
    ) {
        let Some(declared) = self.traits.get(trait_name) else {
            return;
        };
        let source = declared.source;
        let type_bindings = declared.type_bindings.clone();
        let defaults: Vec<&'a Function> = declared
            .members
            .iter()
            .filter(|member| !member.required && !provided.contains(&member.name))
            .map(|member| member.function)
            .collect();
        for function in defaults {
            let callable = Callable {
                receiver: Some(receiver),
                origin: None,
                specialize: Vec::new(),
                initializes: None,
                function,
                source,
                type_bindings: type_bindings.clone(),
            };
            let id = kira_semantics_model::hir::FuncId(self.sigs.len() as u32);
            self.collect_signatures(std::slice::from_ref(&callable));
            self.generic_callables.push((id, callable));
        }
    }

    /// Resolves a generic struct/class constructor. An explicit `Box<Int>(… )`
    /// supplies the arguments directly; otherwise the expected result type or
    /// the types of positional field values provide them. The constructor then
    /// uses the same memberwise/class lowering as a non-generic declaration.
    pub(crate) fn generic_aggregate_for_call(
        &mut self,
        ctx: &crate::analyze::FnCtx,
        name: &str,
        explicit: &[TypeRefId],
        values: &[kira_syntax_model::ast::CallArg],
        expected: Option<Type>,
        span: Span,
    ) -> Option<StructId> {
        let template = self.generic_aggregates.get(name).copied()?;
        if let Some(Type::Struct(id)) = expected
            && self
                .generic_instance_templates
                .get(&id)
                .is_some_and(|text| text == name)
            && explicit.is_empty()
        {
            return Some(id);
        }
        let mut args: Vec<Option<Type>> = vec![None; template.type_params().len()];
        let mut conflicts = vec![false; args.len()];
        if explicit.len() > args.len() {
            self.emit(
                span,
                "KSEM174",
                format!(
                    "generic `{name}` takes {} type argument{}, but {} {} written",
                    args.len(),
                    if args.len() == 1 { "" } else { "s" },
                    explicit.len(),
                    if explicit.len() == 1 { "was" } else { "were" },
                ),
            );
            return None;
        }
        let mut explicit_error = false;
        for (index, &type_ref) in explicit.iter().enumerate() {
            let ty = self.resolve_type_ref(type_ref);
            explicit_error |= ty == Type::Error;
            args[index] = Some(ty);
        }
        if explicit_error {
            return None;
        }
        let actual = self.try_argument_types(ctx, &[], values);
        let parameter_names: Vec<String> = template
            .type_params()
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .collect();
        // Memberwise construction binds labeled values by field name, not by
        // the order in which the labels were written. Class construction uses
        // only fields without defaults, including inherited fields. Inference
        // mirrors both constructor slot rules so a specialization is chosen
        // from the same field each real constructor path will check.
        let fields = self.generic_inference_fields(template);
        let positional_slots: Vec<usize> = match template {
            GenericAggregate::Struct { .. } => (0..fields.len()).collect(),
            GenericAggregate::Class { .. } => fields
                .iter()
                .enumerate()
                .filter_map(|(index, field)| (!field.has_default).then_some(index))
                .collect(),
        };
        let mut next_positional = 0usize;
        for (written, &value) in values.iter().zip(actual.iter()) {
            let slot = match (template, written.label) {
                (GenericAggregate::Struct { .. }, Some(label)) => {
                    let label = self.interner.resolve(label);
                    fields.iter().position(|field| field.name == label)
                }
                (GenericAggregate::Struct { .. }, None) => {
                    let slot = next_positional;
                    next_positional += 1;
                    positional_slots.get(slot).copied()
                }
                // Class labels are ignored by the real constructor, which
                // receives `argument_values` in written order.
                (GenericAggregate::Class { .. }, _) => {
                    let slot = next_positional;
                    next_positional += 1;
                    positional_slots.get(slot).copied()
                }
            };
            if let Some(slot) = slot {
                self.infer_type_ref_with_substitution(
                    fields[slot].type_ref,
                    value,
                    &parameter_names,
                    &mut args,
                    &mut conflicts,
                    &fields[slot].substitution,
                );
            }
        }
        let Some(args): Option<Vec<Type>> = args.into_iter().collect() else {
            self.emit(
                span,
                "KSEM316",
                format!(
                    "cannot infer every type argument of generic `{name}` from this \
                     construction; add an expected type or explicit type arguments"
                ),
            );
            return None;
        };
        if conflicts.iter().any(|conflict| *conflict) {
            self.emit(
                span,
                "KSEM316",
                format!(
                    "generic `{name}` has conflicting inferred type arguments; add an expected \
                     type or explicit type arguments"
                ),
            );
            return None;
        }
        match self.instantiate_aggregate(name, template, &args, span) {
            Type::Struct(id) => {
                self.generic_instance_arguments.insert(
                    id,
                    Instantiation {
                        template: name.to_owned(),
                        arguments: args,
                    },
                );
                Some(id)
            }
            _ => None,
        }
    }
}
