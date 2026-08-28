//! Generic parameter-bound checking.

use super::*;

impl<'a> Analyzer<'a> {
    /// Answers every queued instantiation's bounds, now that the tables the
    /// answers read are final.
    ///
    /// Called twice per analysis — once after drop recording, once after bodies
    /// and synthesized functions have finished minting rows — because an
    /// instantiation can be minted at any point a type resolves. Each entry is
    /// answered once and dropped; the second call sees only what bodies added.
    pub(crate) fn check_pending_generic_bounds(&mut self) {
        if self.pending_bounds.is_empty() {
            return;
        }
        for entry in std::mem::take(&mut self.pending_bounds) {
            let Some(params) = self.generic_parameters(&entry.template) else {
                continue;
            };
            let params: Vec<(String, Vec<TraitRef>)> = params
                .iter()
                .map(|param| {
                    (
                        self.interner.resolve(param.name).to_owned(),
                        param.bounds.clone(),
                    )
                })
                .collect();
            let bindings: TypeBindings = params
                .iter()
                .map(|(name, _)| name.clone())
                .zip(entry.args.iter().copied())
                .collect();
            for ((param_name, bounds), &arg) in params.iter().zip(entry.args.iter()) {
                for bound in bounds {
                    let Some(bound_name) =
                        self.resolve_bound_trait_ref(bound, &bindings, entry.declaration_source)
                    else {
                        // A bound whose name or arguments did not resolve was
                        // diagnosed while the declaration was collected.
                        continue;
                    };
                    let Some(reason) = self.bound_unmet(&bound_name, arg) else {
                        continue;
                    };
                    let spelling = self.type_name(arg);
                    self.source = entry.source;
                    self.emit(
                        entry.span,
                        "KSEM315",
                        format!(
                            "type argument `{spelling}` for parameter `{param_name}` of generic \
                             {} `{}` does not satisfy its bound `{bound_name}`: {reason}",
                            entry.kind, entry.template
                        ),
                    );
                }
            }
        }
    }

    /// Resolves one bound's optional type arguments with the substitution that
    /// belongs to the generic instantiation being checked. The generic
    /// declaration's source is restored afterward so a bound cannot leak its
    /// imports or bindings into the surrounding pass.
    fn resolve_bound_trait_ref(
        &mut self,
        bound: &TraitRef,
        bindings: &TypeBindings,
        declaration_source: SourceId,
    ) -> Option<String> {
        let outer_bindings = std::mem::replace(&mut self.type_bindings, bindings.clone());
        let outer_source = self.source;
        self.source = declaration_source;
        let resolved = self.resolve_trait_ref(bound);
        self.source = outer_source;
        self.type_bindings = outer_bindings;
        resolved
    }

    /// The parameter declarations for any generic source-level template.
    fn generic_parameters(&self, name: &str) -> Option<Vec<kira_syntax_model::ast::TypeParamDecl>> {
        if let Some(template) = self.generic_enums.get(name) {
            return Some(template.decl.type_params.clone());
        }
        if let Some(template) = self.generic_aggregates.get(name) {
            return Some(template.type_params().to_vec());
        }
        if let Some(template) = self
            .traits
            .get(name)
            .filter(|trait_info| !trait_info.type_params.is_empty())
        {
            return Some(template.type_params.clone());
        }
        self.generic_functions
            .get(name)
            .map(|template| template.function.type_params.clone())
    }

    /// Why `arg` fails the bound `bound`, or `None` when it keeps it.
    ///
    /// Three kinds of bound, each answered by the machinery that already owns
    /// the question: a compiler-known derived marker by the structural fact,
    /// `Drop` by whether the type's release runs a user body, and a declared
    /// trait by the conformance table plus its supertrait closure — slice 1a's
    /// answers, reused rather than restated.
    fn bound_unmet(&self, bound: &str, arg: Type) -> Option<String> {
        if crate::traits::is_derived_trait(bound) {
            return self.derived_bound_unmet(bound, arg);
        }
        if bound == crate::traits::DROP {
            let name = self.type_name(arg);
            return (!self.program.types.runs_user_drop(arg))
                .then(|| format!("`{name}` runs no user `Drop` body"));
        }
        let name = self.type_name(arg);
        if !self.conforms_to(arg, bound) {
            return Some(format!(
                "`{name}` does not conform to `{bound}`; add it to the conformance list, or \
                 write `extend {name}: {bound} {{ … }}`"
            ));
        }
        self.supertrait_obligation_unmet(bound, arg, &mut HashSet::new())
            .map(|(unmet, reason)| format!("`{bound}` requires `{unmet}`, and {reason}"))
    }

    /// Why `ty` fails the *derived* marker `bound`, phrased for a use site.
    fn derived_bound_unmet(&self, bound: &str, ty: Type) -> Option<String> {
        let claimed = self.type_name(ty);
        match crate::traits::markers::Marker::from_name(bound) {
            Some(marker) => self.marker_reason(&claimed, ty, marker),
            None => self.not_copyable_reason(&claimed, ty, &mut HashSet::new()),
        }
    }

    /// The first unmet obligation in `trait_name`'s supertrait closure, with
    /// the trait that left it on.
    ///
    /// Direct obligations first, then theirs: a conformance that is itself
    /// checked discharges its own closure, so only the unmet leaf needs naming.
    /// `visited` stops a shared subgraph being walked twice; cycles were
    /// refused where they were declared (`KSEM309`).
    fn supertrait_obligation_unmet(
        &self,
        trait_name: &str,
        ty: Type,
        visited: &mut HashSet<String>,
    ) -> Option<(String, String)> {
        let edges: Vec<String> = self
            .traits
            .get(trait_name)?
            .supertraits
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        for edge in edges {
            if !visited.insert(edge.clone()) {
                continue;
            }
            if let Some(reason) = self.single_conformance_unmet(&edge, ty) {
                return Some((edge, reason));
            }
            if let Some(found) = self.supertrait_obligation_unmet(&edge, ty, visited) {
                return Some(found);
            }
        }
        None
    }

    /// Why `ty` does not carry one obligation — a derived marker, `Drop`, or a
    /// declared trait's row — without walking further.
    fn single_conformance_unmet(&self, name: &str, ty: Type) -> Option<String> {
        let spelled = self.type_name(ty);
        if crate::traits::is_derived_trait(name) {
            return self.derived_trait_unmet(name, ty);
        }
        if name == crate::traits::DROP {
            return (!self.program.types.runs_user_drop(ty))
                .then(|| format!("`{spelled}` runs no user `Drop` body"));
        }
        (!self.conforms_to(ty, name)).then(|| format!("`{spelled}` does not conform to `{name}`"))
    }
}
