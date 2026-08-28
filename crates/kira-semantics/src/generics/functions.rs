//! Generic free-function inference and specialization.

use super::*;

/// Mutable inference state threaded through one generic type reference.
struct TypeInference<'a> {
    /// Generic parameter names in declaration order.
    names: &'a [String],
    /// Inferred concrete types for those parameters.
    bindings: &'a mut [Option<Type>],
    /// Parameters inferred to two different concrete types.
    conflicts: &'a mut [bool],
    /// Substitutions inherited from a generic parent template.
    substitution: &'a [Vec<(String, TypeRefId)>],
    /// Guards recursive parent-parameter substitutions.
    substitutions_in_flight: &'a mut HashSet<(usize, String)>,
}

impl<'a> Analyzer<'a> {
    /// Infers a generic free function's arguments and reserves its concrete
    /// signature. Inference intentionally happens before the ordinary argument
    /// checker: the parameter type is the thing being discovered, so checking
    /// against an unresolved parameter would either guess or produce the old
    /// cascading unknown-type error.
    pub(crate) fn instantiate_generic_function(
        &mut self,
        ctx: &crate::analyze::FnCtx,
        name: &str,
        explicit: &[TypeRefId],
        leading: &[kira_semantics_model::hir::HirExprId],
        args: &[kira_syntax_model::ast::CallArg],
        trailing: &[kira_semantics_model::hir::HirExprId],
    ) -> Option<kira_semantics_model::hir::FuncId> {
        let template = self.generic_functions.get(name).copied()?;
        if !leading.is_empty() {
            return None;
        }
        let explicit_types: Vec<Type> = explicit
            .iter()
            .map(|&arg| self.resolve_type_ref(arg))
            .collect();
        let actual = self.try_argument_types(ctx, leading, args);
        let expected_len = template.function.params.len();
        let mut bindings: Vec<Option<Type>> = vec![None; template.function.type_params.len()];
        let mut conflicts = vec![false; bindings.len()];
        let parameter_names: Vec<String> = template
            .function
            .type_params
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .collect();
        if explicit_types.len() > parameter_names.len() {
            self.emit(
                template.function.name_span,
                "KSEM174",
                format!(
                    "generic function `{name}` takes {} type argument(s), found {}",
                    parameter_names.len(),
                    explicit_types.len()
                ),
            );
            return None;
        }
        if explicit_types.contains(&Type::Error) {
            return None;
        }
        for (index, ty) in explicit_types.into_iter().enumerate() {
            bindings[index] = Some(ty);
        }
        if actual.len() != expected_len {
            // Let the ordinary signature checker report arity after a partial
            // inference attempt; a generic function still needs every type
            // argument it can infer from the arguments that were present.
        }
        for (index, param) in template.function.params.iter().enumerate() {
            let Some(&actual_ty) = actual.get(index) else {
                continue;
            };
            self.infer_type_ref(
                param.ty,
                actual_ty,
                &parameter_names,
                &mut bindings,
                &mut conflicts,
            );
        }
        let Some(args): Option<Vec<Type>> = bindings.into_iter().collect() else {
            self.emit(
                template.function.name_span,
                "KSEM316",
                format!(
                    "cannot infer every type argument of generic function `{name}` from this \
                     call; pass explicit type arguments or use the parameter in an argument"
                ),
            );
            return None;
        };
        if conflicts.iter().any(|conflict| *conflict) {
            self.emit(
                template.function.name_span,
                "KSEM316",
                format!(
                    "generic function `{name}` has conflicting inferred type arguments; pass \
                     explicit type arguments"
                ),
            );
            return None;
        }
        let key = self.mangle(name, &args);
        if let Some(&id) = self.generic_function_instances.get(&key) {
            return Some(id);
        }
        let function_bindings: TypeBindings = parameter_names
            .into_iter()
            .zip(args.iter().copied())
            .collect();
        let callable = Callable {
            receiver: None,
            origin: None,
            specialize: Vec::new(),
            initializes: None,
            function: template.function,
            source: template.source,
            type_bindings: function_bindings,
        };
        let id = kira_semantics_model::hir::FuncId(self.sigs.len() as u32);
        self.collect_signatures(std::slice::from_ref(&callable));
        self.generic_function_instances.insert(key.clone(), id);
        self.generic_callables.push((id, callable));
        if template
            .function
            .type_params
            .iter()
            .any(|param| !param.bounds.is_empty())
        {
            self.pending_bounds.push(PendingBoundCheck {
                key,
                template: name.to_owned(),
                args,
                source: self.source,
                declaration_source: template.source,
                span: template.function.name_span,
                kind: "function",
            });
        }
        let _ = trailing;
        Some(id)
    }

    /// Unifies a written parameter type with the concrete type of one argument.
    /// The boolean result is deliberately ignored by callers: a non-generic
    /// portion is checked by the ordinary call path, while a conflicting type
    /// parameter is represented by two different bindings and rejected below.
    pub(crate) fn infer_type_ref(
        &self,
        type_ref: TypeRefId,
        actual: Type,
        names: &[String],
        bindings: &mut [Option<Type>],
        conflicts: &mut [bool],
    ) {
        self.infer_type_ref_with_substitution(type_ref, actual, names, bindings, conflicts, &[]);
    }

    /// Unifies a written type while replacing a parent template's parameters
    /// with the type references written at the child site. The replacement is
    /// applied once at a parameter leaf; applying the same map again would
    /// recurse forever for the common `Parent<Value>` case where both names
    /// are spelled `Value`.
    pub(crate) fn infer_type_ref_with_substitution(
        &self,
        type_ref: TypeRefId,
        actual: Type,
        names: &[String],
        bindings: &mut [Option<Type>],
        conflicts: &mut [bool],
        substitution: &[Vec<(String, TypeRefId)>],
    ) {
        let mut substitutions_in_flight = HashSet::new();
        let mut state = TypeInference {
            names,
            bindings,
            conflicts,
            substitution,
            substitutions_in_flight: &mut substitutions_in_flight,
        };
        self.infer_type_ref_inner(type_ref, actual, &mut state);
    }

    fn infer_type_ref_inner(
        &self,
        type_ref: TypeRefId,
        actual: Type,
        state: &mut TypeInference<'_>,
    ) {
        match self.tree.type_ref(type_ref) {
            kira_syntax_model::ast::TypeRef::Named { name, .. } => {
                let written = self.interner.resolve(*name);
                if let Some((_, replacement)) = state
                    .substitution
                    .first()
                    .into_iter()
                    .flat_map(|layer| layer.iter().rev())
                    .find(|(parameter, _)| parameter == written)
                {
                    let marker = (state.substitution.len(), written.to_owned());
                    if state.substitutions_in_flight.insert(marker.clone()) {
                        let substitution = state.substitution;
                        state.substitution = &substitution[1..];
                        self.infer_type_ref_inner(*replacement, actual, state);
                        state.substitution = substitution;
                        state.substitutions_in_flight.remove(&marker);
                    }
                    return;
                }
                if let Some(index) = state.names.iter().position(|name| name == written) {
                    match state.bindings[index] {
                        None => state.bindings[index] = Some(actual),
                        Some(previous) if previous != actual => state.conflicts[index] = true,
                        _ => {}
                    }
                }
            }
            kira_syntax_model::ast::TypeRef::Array { element, .. } => {
                if let Type::Array(id) = actual
                    && let Some(element_ty) = self.program.types.arrays().element(id)
                {
                    self.infer_type_ref_inner(*element, element_ty, state);
                }
            }
            kira_syntax_model::ast::TypeRef::Generic { name, args, .. } => {
                let written = self.interner.resolve(*name);
                let template = written
                    .rsplit_once('.')
                    .map_or(written, |(_, member)| member);
                let instantiation = match actual {
                    Type::Enum(id) => self.program.types.enums().instantiation(id),
                    Type::Struct(id) => self.generic_instance_arguments.get(&id),
                    _ => None,
                };
                let Some(instantiation) = instantiation else {
                    return;
                };
                if instantiation.template != template || instantiation.arguments.len() != args.len()
                {
                    return;
                }
                for (&written_arg, &actual_arg) in args.iter().zip(instantiation.arguments.iter()) {
                    self.infer_type_ref_inner(written_arg, actual_arg, state);
                }
            }
            _ => {}
        }
    }
}
