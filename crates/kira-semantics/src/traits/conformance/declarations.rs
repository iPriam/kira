use super::*;

impl<'a> Analyzer<'a> {
    /// Registers every `trait` declaration the program writes.
    ///
    /// Runs before any type table is built: a trait's *name* has to be known
    /// while structs, classes, enums, and construct families are being
    /// declared, because Kira has one type namespace and each of those has to
    /// be able to lose a collision to a trait.
    pub(crate) fn collect_traits(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Trait(declaration) = item else {
                continue;
            };
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            let supertraits = declaration
                .supertraits
                .iter()
                .map(|entry| SupertraitRef {
                    name: self.interner.resolve(entry.name).to_owned(),
                    span: entry.span,
                    args: entry.args.clone(),
                })
                .collect();
            if is_builtin_trait(&name) {
                self.emit(
                    declaration.name_span,
                    "KSEM288",
                    format!(
                        "`{name}` is a trait the compiler already knows, so it cannot be \
                         declared: what it means is decided by a type's own members, not by a \
                         written body"
                    ),
                );
                continue;
            }
            if let Some(kind) = self.name_declared_elsewhere(&name, source, declaration.name_span) {
                self.emit(
                    declaration.name_span,
                    "KSEM288",
                    format!(
                        "`{name}` is already defined as {kind}, so this trait cannot take the \
                         same name: a type name means exactly one declaration"
                    ),
                );
                continue;
            }
            let members = declaration
                .members
                .iter()
                .map(|member| TraitMemberInfo {
                    name: self.interner.resolve(member.function.name).to_owned(),
                    required: !member.has_body,
                    function: &member.function,
                })
                .collect();
            let key = self.template_key(source, &name);
            self.traits.insert(
                key,
                TraitInfo {
                    source,
                    type_params: declaration.type_params.clone(),
                    type_bindings: Vec::new(),
                    supertraits,
                    members,
                },
            );
        }
        self.check_supertrait_graph();
    }

    /// Checks every supertrait clause once every trait name is known.
    ///
    /// Two questions, both answerable from the graph alone: whether each name
    /// is a trait at all, and whether following the clauses ever returns to
    /// where it started. A cycle is refused because a requirement that
    /// eventually requires itself can never be discharged — no conformance
    /// list finishes it.
    fn check_supertrait_graph(&mut self) {
        let names: Vec<String> = self.traits.keys().cloned().collect();
        let mut unknown = Vec::new();
        let here = self.source;
        for name in &names {
            let source = self.traits[name].source;
            // A supertrait is written as a bare name in the trait's own file,
            // so it resolves against that file's package and imports; the
            // stored name becomes the key it resolved to.
            self.source = source;
            let mut resolved: Vec<(usize, String)> = Vec::new();
            for (index, entry) in self.traits[name].supertraits.iter().enumerate() {
                if is_builtin_trait(&entry.name) {
                    continue;
                }
                match self.visible_trait_key(&entry.name) {
                    Some(key) => resolved.push((index, key)),
                    None => unknown.push((source, entry.span, name.clone(), entry.name.clone())),
                }
            }
            if let Some(declared) = self.traits.get_mut(name) {
                for (index, key) in resolved {
                    declared.supertraits[index].name = key;
                }
            }
        }
        self.source = here;
        for (source, span, name, super_name) in unknown {
            self.source = source;
            self.emit(
                span,
                "KSEM308",
                format!(
                    "`{super_name}` is not a trait, so `{name}` cannot require it: a supertrait \
                     names a trait every conforming type must also claim"
                ),
            );
        }
        for name in &names {
            let Some((span, cycle)) = self.supertrait_cycle_through(name) else {
                continue;
            };
            self.source = self.traits[name].source;
            self.emit(
                span,
                "KSEM309",
                format!(
                    "`{name}` requires itself through {cycle}: a supertrait is an obligation a \
                     conforming type discharges, and one that comes back around can never be \
                     discharged"
                ),
            );
        }
    }

    /// The clause `name` leaves on to return to itself, and the chain it takes,
    /// or `None` when every clause terminates.
    fn supertrait_cycle_through(&self, name: &str) -> Option<(Span, String)> {
        let mut chain = vec![name.to_owned()];
        let mut visited = HashSet::new();
        if !self.walk_supertraits_back_to(name, &mut chain, &mut visited) {
            return None;
        }
        let leaves_on = chain.get(1)?;
        let span = self
            .traits
            .get(name)?
            .supertraits
            .iter()
            .find(|entry| &entry.name == leaves_on)?
            .span;
        Some((span, chain.join(" -> ")))
    }

    /// Extends `chain` along supertrait edges until it returns to `target`.
    ///
    /// `visited` holds the traits already ruled out, so a shared subgraph is
    /// walked once however many clauses reach it.
    fn walk_supertraits_back_to(
        &self,
        target: &str,
        chain: &mut Vec<String>,
        visited: &mut HashSet<String>,
    ) -> bool {
        let Some(declared) = chain.last().and_then(|here| self.traits.get(here)) else {
            return false;
        };
        let edges: Vec<String> = declared
            .supertraits
            .iter()
            .map(|entry| entry.name.clone())
            .collect();
        for next in edges {
            if next == target {
                chain.push(next);
                return true;
            }
            if !visited.insert(next.clone()) {
                continue;
            }
            chain.push(next);
            if self.walk_supertraits_back_to(target, chain, visited) {
                return true;
            }
            chain.pop();
        }
        false
    }

    /// What else already declares `name` in the package `source` belongs to, or
    /// `None` when the name is free there.
    ///
    /// Asked of the *syntax* rather than of a type table, because traits are
    /// collected before any table exists. `span` identifies the declaration
    /// asking, so a trait does not report itself.
    fn name_declared_elsewhere(
        &self,
        name: &str,
        source: SourceId,
        span: Span,
    ) -> Option<&'static str> {
        let package = self.imports.package_of(source);
        self.tree.items_with_source().find_map(|(other, item)| {
            if self.imports.package_of(other) != package {
                return None;
            }
            let (kind, declared, declared_span) = match item {
                Item::Struct(it) => ("a struct", it.name, it.name_span),
                Item::Class(it) => ("a class", it.name, it.name_span),
                Item::Enum(it) => ("an enum", it.name, it.name_span),
                Item::TypeAlias(it) => ("a type alias", it.name, it.name_span),
                Item::Construct(it) => match it.kind {
                    ConstructKind::Family => ("a construct family", it.name, it.name_span),
                    ConstructKind::Backed { .. } => ("a declaration", it.name, it.name_span),
                },
                Item::Trait(it) => ("a trait", it.name, it.name_span),
                _ => return None,
            };
            let repeat = declared_span == span && other == source;
            (!repeat && self.interner.resolve(declared) == name).then_some(kind)
        })
    }
}
