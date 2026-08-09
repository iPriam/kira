//! Construct family inheritance: `construct Child extends Parent`.
//!
//! A family that extends another takes on its parent's conformance surface —
//! requirements, methods, and member types — and every declaration backed by the
//! child also becomes a variant of the parent's `Any Parent` enum. Those two
//! halves are what make inheritance worth having: the first means a child's
//! declarations must satisfy what the parent asked for, and the second means a
//! runtime can hold `[Any Parent]` and drive declarations written against any
//! child without knowing the children exist.
//!
//! Merging the surface downward, rather than searching upward at every lookup,
//! is what keeps the rest of the analyzer unchanged: conformance checking,
//! dispatcher synthesis and member typing all read one family's maps and never
//! learn that a parent was involved.

use std::collections::{BTreeMap, HashSet};

use kira_semantics_model::{EnumId, StructId, Type};
use kira_source::{SourceId, Span};

use crate::analyze::Analyzer;

/// One family's `extends` clause, resolved to names.
struct Parents {
    child: String,
    parents: Vec<(String, Span)>,
    source: SourceId,
}

impl<'a> Analyzer<'a> {
    /// Resolves every `extends` clause and merges each parent's surface into its
    /// children.
    ///
    /// Runs between family headers and backed declarations: the headers give
    /// every family its maps, and a backed declaration is checked against the
    /// merged surface rather than the written one.
    pub(crate) fn inherit_construct_families(&mut self) {
        let declared = self.collect_family_parents();
        let order = self.family_inheritance_order(&declared);
        for child in order {
            let Some(row) = declared.iter().find(|row| row.child == child) else {
                continue;
            };
            self.source = row.source;
            let parents: Vec<String> = row.parents.iter().map(|(name, _)| name.clone()).collect();
            for parent in parents {
                self.merge_family_surface(&child, &parent);
            }
        }
    }

    /// Reads each family's `extends` clause, refusing a parent that is not a
    /// family and a family that extends itself.
    fn collect_family_parents(&mut self) -> Vec<Parents> {
        let mut rows = Vec::new();
        for (source, declaration) in self.family_declarations() {
            if declaration.extends.is_empty() {
                continue;
            }
            self.source = source;
            let child = self.interner.resolve(declaration.name).to_owned();
            let mut parents = Vec::new();
            let mut seen = HashSet::new();
            for parent in &declaration.extends {
                let name = self.interner.resolve(parent.name).to_owned();
                if name == child {
                    self.emit(
                        parent.span,
                        "KSEM204",
                        format!("construct family `{child}` extends itself"),
                    );
                    continue;
                }
                if !self.construct_families.contains_key(&name) {
                    self.emit(
                        parent.span,
                        "KSEM200",
                        format!(
                            "`{child}` extends `{name}`, which is not a construct family in scope"
                        ),
                    );
                    continue;
                }
                if !seen.insert(name.clone()) {
                    self.emit(
                        parent.span,
                        "KSEM204",
                        format!("construct family `{child}` extends `{name}` more than once"),
                    );
                    continue;
                }
                parents.push((name, parent.span));
            }
            rows.push(Parents {
                child,
                parents,
                source,
            });
        }
        rows
    }

    /// Orders families so a parent is merged before its children, reporting any
    /// family caught in a cycle.
    ///
    /// A cycle is refused rather than broken at an arbitrary edge: the surface a
    /// declaration must satisfy would otherwise depend on which family the
    /// analyzer happened to reach first.
    fn family_inheritance_order(&mut self, declared: &[Parents]) -> Vec<String> {
        let mut pending: BTreeMap<&str, &Parents> = declared
            .iter()
            .map(|row| (row.child.as_str(), row))
            .collect();
        let mut ordered = Vec::new();
        let mut done: HashSet<String> = HashSet::new();
        while !pending.is_empty() {
            let ready: Vec<String> = pending
                .values()
                .filter(|row| {
                    row.parents.iter().all(|(name, _)| {
                        done.contains(name) || !pending.contains_key(name.as_str())
                    })
                })
                .map(|row| row.child.clone())
                .collect();
            if ready.is_empty() {
                for row in pending.values() {
                    let Some((_, span)) = row.parents.first() else {
                        continue;
                    };
                    self.source = row.source;
                    self.emit(
                        *span,
                        "KSEM205",
                        format!(
                            "construct family `{}` inherits from itself through a cycle",
                            row.child
                        ),
                    );
                }
                break;
            }
            for child in ready {
                pending.remove(child.as_str());
                done.insert(child.clone());
                ordered.push(child);
            }
        }
        ordered
    }

    /// Copies everything `parent` states onto `child` that `child` does not
    /// state itself.
    ///
    /// The child always wins, which is what overriding a parent's member means.
    /// A method the child redeclares keeps the child's body and the child's
    /// requiredness — a child may discharge a parent's `@Required function` by
    /// writing one with a body, and every declaration backed by the child then
    /// inherits that body.
    fn merge_family_surface(&mut self, child: &str, parent: &str) {
        let Some(inherited) = self.construct_families.get(parent).cloned() else {
            return;
        };
        let Some(info) = self.construct_families.get_mut(child) else {
            return;
        };
        for name in inherited.required {
            if !info.required.contains(&name) {
                info.required.push(name);
            }
        }
        for (name, method) in inherited.methods {
            info.methods.entry(name).or_insert(method);
        }
        for (name, field) in inherited.field_members {
            info.field_members.entry(name).or_insert(field);
        }
        for field in inherited.stored_fields {
            if !info
                .stored_fields
                .iter()
                .any(|existing| existing.name == field.name)
            {
                info.stored_fields.push(field);
            }
        }
        for (name, written) in inherited.member_types {
            info.member_types.entry(name).or_insert(written);
        }
        // The parent's own ancestors come with it, so a child's list is
        // transitive without walking the chain at every read.
        for ancestor in std::iter::once(parent.to_owned()).chain(inherited.parents) {
            if !info.parents.contains(&ancestor) {
                info.parents.push(ancestor);
            }
        }
    }

    /// Checks every member a child family redeclares against the parent's.
    ///
    /// A child may only make a promise *more* specific, never different. Runs
    /// after family signatures resolve, because that is when there are types to
    /// compare — the merge itself happens while they are still written forms.
    pub(crate) fn check_family_overrides(&mut self) {
        for (source, declaration) in self.family_declarations() {
            if declaration.extends.is_empty() {
                continue;
            }
            self.source = source;
            let child = self.interner.resolve(declaration.name).to_owned();
            for parent in &declaration.extends {
                let parent_name = self.interner.resolve(parent.name).to_owned();
                for method in &declaration.methods {
                    let member = self.interner.resolve(method.function.name).to_owned();
                    self.check_method_override(
                        &child,
                        &parent_name,
                        &member,
                        method.function.name_span,
                    );
                }
                for field in &declaration.fields {
                    let member = self.interner.resolve(field.name).to_owned();
                    self.check_field_override(&child, &parent_name, &member, field.name_span);
                }
            }
        }
    }

    /// Compares one redeclared method against the parent's signature.
    fn check_method_override(&mut self, child: &str, parent: &str, member: &str, span: Span) {
        let Some(promised) = self
            .construct_families
            .get(parent)
            .and_then(|info| info.methods.get(member))
        else {
            return;
        };
        let (promised_params, promised_ownership, promised_result) = (
            promised.params.clone(),
            promised.ownership.clone(),
            promised.constrained_result(),
        );
        let Some(written) = self
            .construct_families
            .get(child)
            .and_then(|info| info.methods.get(member))
        else {
            return;
        };
        let (params, ownership, result) = (
            written.params.clone(),
            written.ownership.clone(),
            written.result,
        );
        if params.len() != promised_params.len() {
            self.emit(
                span,
                "KSEM206",
                format!(
                    "`{child}` redeclares `{member}` with {} parameter(s), and `{parent}` declares \
                     it with {}",
                    params.len(),
                    promised_params.len()
                ),
            );
            return;
        }
        // A parameter is the one position a child must not narrow. Everything
        // holding an `Any {parent}` may pass whatever the parent's signature
        // accepts, and a child that asked for less would refuse a value the
        // parent promised to take.
        for (index, (written, promised)) in params.iter().zip(&promised_params).enumerate() {
            if written != promised || ownership.get(index) != promised_ownership.get(index) {
                let (written, promised) = (self.type_name(*written), self.type_name(*promised));
                self.emit(
                    span,
                    "KSEM206",
                    format!(
                        "parameter {} of `{member}` is `{written}` here and `{promised}` in \
                         `{parent}`; a child may make a result more specific but must accept \
                         everything the family it extends accepts",
                        index + 1
                    ),
                );
                return;
            }
        }
        let Some(promised_result) = promised_result else {
            return;
        };
        if !self.narrows_to(result, promised_result) {
            let (result, promised) = (self.type_name(result), self.type_name(promised_result));
            self.emit(
                span,
                "KSEM206",
                format!(
                    "`{member}` returns `{result}` here and `{promised}` in `{parent}`; a child may \
                     only make a result more specific"
                ),
            );
        }
    }

    /// Compares one redeclared value member against the parent's type.
    fn check_field_override(&mut self, child: &str, parent: &str, member: &str, span: Span) {
        let Some(promised) = self
            .construct_families
            .get(parent)
            .and_then(|info| info.field_members.get(member))
            .map(|field| field.result)
        else {
            return;
        };
        let Some(written) = self
            .construct_families
            .get(child)
            .and_then(|info| info.field_members.get(member))
            .map(|field| field.result)
        else {
            return;
        };
        if !self.narrows_to(written, promised) {
            let (written, promised) = (self.type_name(written), self.type_name(promised));
            self.emit(
                span,
                "KSEM206",
                format!(
                    "`{member}` is `{written}` here and `{promised}` in `{parent}`; a child may \
                     only make a member more specific"
                ),
            );
        }
    }

    /// Whether `written` is `promised` or something more specific than it.
    ///
    /// `Any` is what everything narrows from. A family type narrows to a family
    /// that extends it and to any declaration backed by one, which is what lets a
    /// parent state `Any Widget` and a child answer with the one declaration it
    /// will always produce.
    fn narrows_to(&self, written: Type, promised: Type) -> bool {
        if written == promised || promised == Type::Any || written == Type::Error {
            return true;
        }
        if self.is_subclass_of(written, promised) {
            return true;
        }
        let Type::Enum(promised) = promised else {
            return false;
        };
        if !self.is_construct_family_type(promised) {
            return false;
        }
        match written {
            Type::Enum(written) => self
                .construct_family_names
                .get(&written)
                .map(|name| self.construct_family_ancestors(name))
                .is_some_and(|ancestors| {
                    ancestors
                        .iter()
                        .any(|name| self.construct_family_type(name) == Some(promised))
                }),
            Type::Struct(written) => self
                .constructs
                .get(&written)
                .is_some_and(|info| info.families.iter().any(|(id, _)| *id == promised)),
            _ => false,
        }
    }

    /// Records `id` as a variant of `family` and of every family it extends,
    /// returning the enum and tag for each.
    ///
    /// A declaration reachable through a parent has to be a variant *of* that
    /// parent: `Any Parent` is an enum, and a value it cannot name is a value it
    /// cannot hold. Registering here rather than widening at the use site is
    /// what lets a parent's dispatcher branch over children it never saw.
    pub(crate) fn register_family_variant(
        &mut self,
        family: &str,
        id: StructId,
    ) -> Vec<(EnumId, u32)> {
        let mut registered = Vec::new();
        let reachable: Vec<String> = std::iter::once(family.to_owned())
            .chain(self.construct_family_ancestors(family))
            .collect();
        for name in reachable {
            if let Some(info) = self.construct_families.get_mut(&name) {
                let tag = info.variants.len() as u32;
                info.variants
                    .push(super::ConstructVariant { struct_id: id, tag });
                registered.push((info.enum_id, tag));
            }
        }
        registered
    }

    /// Every family `name` reaches through `extends`, nearest first.
    ///
    /// The list is already transitive: merging runs parents-before-children, so
    /// a child's own list carries whatever its parents had reached.
    pub(crate) fn construct_family_ancestors(&self, name: &str) -> Vec<String> {
        self.construct_families
            .get(name)
            .map(|info| info.parents.clone())
            .unwrap_or_default()
    }
}
