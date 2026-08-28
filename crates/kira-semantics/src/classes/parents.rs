use super::*;

impl Analyzer<'_> {
    /// Resolves the `extends` list to ids, reporting unknown and duplicated
    /// parents.
    pub(crate) fn resolve_parents(&mut self, declaration: &ClassDecl) -> Vec<StructId> {
        let mut parents: Vec<StructId> = Vec::new();
        for parent in &declaration.parents {
            let written = self.interner.resolve(parent.name).to_owned();
            let resolved = if !parent.type_args.is_empty() || self.is_generic_aggregate(&written) {
                self.resolve_generic_instantiation(
                    parent.name,
                    parent.span,
                    &parent.type_args,
                    parent.span,
                    &NameContext::Ordinary,
                )
            } else {
                self.visible_struct(&written)
                    .map(Type::Struct)
                    .unwrap_or(Type::Error)
            };
            let Type::Struct(id) = resolved else {
                // A parent dropped for a cycle already produced its diagnostic;
                // reporting it missing here would blame the wrong declaration.
                if resolved == Type::Error && !self.unflattenable_classes.contains(&written) {
                    self.emit(
                        parent.span,
                        "KSEM003",
                        format!("unknown parent type `{written}`"),
                    );
                }
                continue;
            };
            if parents.contains(&id) {
                self.emit(
                    parent.span,
                    "KSEM065",
                    format!("`{written}` is already a parent of this class"),
                );
                continue;
            }
            parents.push(id);
        }
        parents
    }

    /// `id` together with everything it inherits from, transitively.
    pub(crate) fn ancestors_of(&self, id: StructId) -> BTreeSet<StructId> {
        let mut all = BTreeSet::new();
        all.insert(id);
        if let Some(info) = self.classes.get(&id) {
            all.extend(info.ancestors.iter().copied());
        }
        all
    }

    /// Whether `descendant` inherits from `ancestor`.
    ///
    /// `own` is the class being declared, whose ancestors are not in the table
    /// yet, so they are passed alongside.
    pub(super) fn inherits_from(
        &self,
        descendant: StructId,
        ancestor: StructId,
        own: StructId,
        own_ancestors: &BTreeSet<StructId>,
    ) -> bool {
        if descendant == own {
            return own_ancestors.contains(&ancestor);
        }
        self.classes
            .get(&descendant)
            .is_some_and(|info| info.ancestors.contains(&ancestor))
    }
}
