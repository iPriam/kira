//! Flattening classes into structs.
//!
//! A class is not a new runtime shape. It is a struct whose fields are its
//! parents' fields followed by its own, and whose methods are one copy of every
//! method any ancestor declares, each analyzed with `self` typed as *this*
//! class. Nothing below semantics learns classes exist: the struct table, the
//! HIR, the IR, both compilers, and the wasm lowering see a struct.
//!
//! That per-class copy is the whole design. Because `self` is always statically
//! the concrete class, a call to an overridden method resolves the same way
//! whether a backend dispatches statically or virtually — so the vm/llvm
//! divergence the oracle documents for inherited `self.m()` cannot arise here.
//! See `.codex/work/classes.md`.
//!
//! Nothing here admits subtyping: a class instance's static type is always its
//! dynamic type, which is what makes the per-class copy total.

mod exprs;

use std::collections::{BTreeSet, HashMap};

use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::{ClassDecl, Item};

use crate::analyze::{Analyzer, FieldDefault};
use crate::types::{AggregateKind, NameContext};

/// How a member name resolved against one class's inherited members.
///
/// The ambiguous case is a real outcome rather than an error to raise on the
/// spot: `ClsCombo` inheriting `v` from two parents is only a problem where
/// someone writes bare `v`, and qualifying it (`ClsAlpha.v`) stays legal.
#[derive(Debug, Clone)]
pub(crate) enum Member<T> {
    /// Exactly one most-derived definition.
    One(T),
    /// Several definitions, none more derived than the others. Carries the
    /// owners, in declaration order, so the diagnostic can name them.
    Ambiguous(Vec<StructId>),
}

/// What a `Name.member` qualifier turned out to be.
///
/// The rejected case is distinct from "not a type" because it has already been
/// reported: treating it as an ordinary expression would blame the qualifier a
/// second time, as an undefined name.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Qualifier {
    /// A type this method's receiver inherits from.
    Parent(StructId),
    /// A type name that cannot qualify here; the diagnostic was emitted.
    Rejected,
    /// Not a type name at all — an ordinary expression.
    NotAType,
}

/// One method a type declares itself.
#[derive(Debug, Clone)]
pub(crate) struct OwnMethod {
    /// The method name as written.
    pub(crate) name: String,
    /// How many parameters it declares, receiver excluded. Compared when an
    /// `override` claims to match it.
    pub(crate) arity: usize,
}

/// Everything analysis knows about one class beyond its struct shape.
#[derive(Debug, Clone, Default)]
pub(crate) struct ClassInfo {
    /// Every transitive ancestor, self excluded. Decides which of two competing
    /// definitions is the more derived.
    pub(crate) ancestors: BTreeSet<StructId>,
    /// Which type declared each flattened slot, and under what name, indexed by
    /// slot. This is what lets a child inherit a parent's already-qualified
    /// field without re-deriving where it came from.
    pub(crate) slot_origin: Vec<(StructId, String)>,
    /// Bare field name to its slot in the flattened struct.
    pub(crate) bare_fields: HashMap<String, Member<u32>>,
    /// `(owner, field name)` to its slot, for parent-qualified reads.
    pub(crate) qualified_fields: HashMap<(StructId, String), u32>,
    /// Bare method name to the ancestor whose body it names.
    pub(crate) bare_methods: HashMap<String, Member<StructId>>,
    /// Every `(owner, method name)` pair this class can name.
    pub(crate) qualified_methods: BTreeSet<(StructId, String)>,
    /// Slots the constructor fills positionally: the fields with no default, in
    /// flattened order.
    pub(crate) required_slots: Vec<u32>,
}

/// One field of a class being flattened, before it becomes a [`FieldDef`].
struct FlatField {
    /// The type that declared it; `None` for a field this class declares
    /// itself, whose id does not exist until the class is declared.
    owner: Option<StructId>,
    /// The name as written, before any qualification.
    plain: String,
    /// The storage name in the flattened struct.
    storage: String,
    ty: Type,
    mutable: bool,
    default: Option<FieldDefault>,
}

impl<'a> Analyzer<'a> {
    /// Declares every class, flattening each into the struct table.
    ///
    /// Runs after [`collect_structs`](Analyzer::collect_structs) because a
    /// class may extend a struct, and in dependency order among classes so a
    /// parent is always flattened before its child.
    pub(crate) fn collect_classes(&mut self) {
        let declarations = self.class_declarations();
        for index in self.class_order(&declarations) {
            let (source, declaration) = declarations[index];
            self.source = source;
            self.declare_class(declaration);
        }
    }

    /// Every class declaration in the program, with the file it was written in.
    ///
    /// Borrowed from the tree rather than from `self`, so declaring one class
    /// can mutate the analyzer while the list stays live.
    fn class_declarations(&self) -> Vec<(SourceId, &'a ClassDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Class(declaration) => Some((source, declaration)),
                _ => None,
            })
            .collect()
    }
}

impl Analyzer<'_> {
    /// Orders class declarations so every class follows its parents.
    ///
    /// Reports one KSEM064 per cycle, at the edge that closes it, and drops
    /// every class in that cycle from the order: a cycle has no valid
    /// flattening, so `class Left extends Right` / `class Right extends Left`
    /// declares neither. Only the closing class is blamed; the rest of the
    /// cycle goes undeclared and its uses are reported by the orphan
    /// propagation pass instead of each earning a cycle diagnostic of its own.
    fn class_order(&mut self, declarations: &[(SourceId, &ClassDecl)]) -> Vec<usize> {
        let by_name: HashMap<&str, usize> = declarations
            .iter()
            .enumerate()
            .map(|(index, (_, declaration))| (self.interner.resolve(declaration.name), index))
            .collect();
        // Iterative DFS, three-colour: 0 unvisited, 1 on the current path, 2
        // finished. An edge back to a node still on the path is the cycle.
        let mut colour = vec![0u8; declarations.len()];
        let mut cyclic = vec![false; declarations.len()];
        let mut order = Vec::with_capacity(declarations.len());
        for root in 0..declarations.len() {
            if colour[root] != 0 {
                continue;
            }
            colour[root] = 1;
            let mut stack = vec![(root, 0usize)];
            while let Some((node, cursor)) = stack.pop() {
                let parents = &declarations[node].1.parents;
                if cursor == parents.len() {
                    colour[node] = 2;
                    if !cyclic[node] {
                        order.push(node);
                    }
                    continue;
                }
                stack.push((node, cursor + 1));
                let parent = self.interner.resolve(parents[cursor].name);
                // A parent that names no class is a struct or an unknown name;
                // `resolve_parents` is what reports the latter.
                let Some(&next) = by_name.get(parent) else {
                    continue;
                };
                match colour[next] {
                    0 => {
                        colour[next] = 1;
                        stack.push((next, 0));
                    }
                    1 => {
                        let name = self.interner.resolve(declarations[node].1.name).to_owned();
                        let span = declarations[node].1.name_span;
                        self.source = declarations[node].0;
                        self.emit(
                            span,
                            "KSEM064",
                            format!("class `{name}` inherits from itself through its parents"),
                        );
                        cyclic[node] = true;
                    }
                    _ => {}
                }
            }
        }
        // A class whose parent was dropped cannot be flattened either, and its
        // parent's absence is not a second mistake to report. `order` is
        // parents-first, so one pass propagates.
        let mut dropped: BTreeSet<String> = declarations
            .iter()
            .zip(cyclic.iter())
            .filter(|(_, cyclic)| **cyclic)
            .map(|((_, declaration), _)| self.interner.resolve(declaration.name).to_owned())
            .collect();
        order.retain(|index| {
            let declaration = declarations[*index].1;
            let orphaned = declaration
                .parents
                .iter()
                .any(|parent| dropped.contains(self.interner.resolve(parent.name)));
            if orphaned {
                dropped.insert(self.interner.resolve(declaration.name).to_owned());
            }
            !orphaned
        });
        self.unflattenable_classes = dropped;
        order
    }

    /// Flattens one class and adds it to the struct table.
    fn declare_class(&mut self, declaration: &ClassDecl) {
        let name = self.interner.resolve(declaration.name).to_owned();
        let parents = self.resolve_parents(declaration);
        let ancestors: BTreeSet<StructId> = parents
            .iter()
            .flat_map(|parent| self.ancestors_of(*parent))
            .collect();
        let flat = self.flatten_fields(declaration, &name, &parents);
        let mut fields = Vec::with_capacity(flat.len());
        let mut defaults = Vec::with_capacity(flat.len());
        let mut pending: Vec<(Option<StructId>, String, Option<FieldDefault>)> =
            Vec::with_capacity(flat.len());
        for field in flat {
            pending.push((field.owner, field.plain, field.default));
            defaults.push(field.default);
            fields.push(FieldDef {
                name: field.storage,
                ty: field.ty,
                mutable: field.mutable,
            });
        }
        let Some(id) = self
            .program
            .types
            .structs_mut()
            .declare(StructDef { name, fields })
        else {
            let name = self.interner.resolve(declaration.name).to_owned();
            self.emit(
                declaration.name_span,
                "KSEM004",
                format!("class `{name}` is already defined"),
            );
            return;
        };
        // Pushed only on success, which is what keeps `struct_defaults` indexed
        // by the ids the table mints — classes and structs share that table.
        self.struct_defaults.push(defaults);
        let mut info = ClassInfo {
            ancestors,
            ..ClassInfo::default()
        };
        for (slot, (owner, plain, default)) in pending.into_iter().enumerate() {
            // A field this class declares itself is owned by the id just minted.
            let owner = owner.unwrap_or(id);
            if default.is_none() {
                info.required_slots.push(slot as u32);
            }
            info.qualified_fields
                .insert((owner, plain.clone()), slot as u32);
            info.slot_origin.push((owner, plain));
        }
        self.record_bare_fields(&mut info);
        self.own_methods.insert(
            id,
            declaration
                .methods
                .iter()
                .map(|method| OwnMethod {
                    name: self.interner.resolve(method.function.name).to_owned(),
                    arity: method.function.params.len(),
                })
                .collect(),
        );
        self.resolve_methods(id, &parents, &mut info);
        self.classes.insert(id, info);
        self.check_overrides(declaration, id);
    }

    /// Resolves the `extends` list to ids, reporting unknown and duplicated
    /// parents.
    fn resolve_parents(&mut self, declaration: &ClassDecl) -> Vec<StructId> {
        let mut parents: Vec<StructId> = Vec::new();
        for parent in &declaration.parents {
            let written = self.interner.resolve(parent.name).to_owned();
            let Some(id) = self.visible_struct(&written) else {
                // A parent dropped for a cycle already produced its diagnostic;
                // reporting it missing here would blame the wrong declaration.
                if !self.unflattenable_classes.contains(&written) {
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
    fn ancestors_of(&self, id: StructId) -> BTreeSet<StructId> {
        let mut all = BTreeSet::new();
        all.insert(id);
        if let Some(info) = self.classes.get(&id) {
            all.extend(info.ancestors.iter().copied());
        }
        all
    }

    /// Builds the flattened field list: every parent's fields, in order, then
    /// this class's own, with `override let` rewriting an inherited default.
    fn flatten_fields(
        &mut self,
        declaration: &ClassDecl,
        name: &str,
        parents: &[StructId],
    ) -> Vec<FlatField> {
        let mut flat: Vec<FlatField> = Vec::new();
        for parent in parents {
            self.inherit_fields(*parent, &mut flat);
        }
        for field in &declaration.fields {
            let plain = self.interner.resolve(field.name).to_owned();
            if flat.iter().any(|existing| existing.plain == plain) {
                // Redeclaring an inherited name without `override` would give
                // the class two slots spelled the same.
                self.emit(
                    field.name_span,
                    "KSEM074",
                    format!(
                        "`{plain}` is already inherited; write `override let {plain} = ...` to \
                         give it a new default"
                    ),
                );
                continue;
            }
            let context = NameContext::Field {
                owner_kind: AggregateKind::Class,
                owner: name.to_owned(),
            };
            let ty = self.resolve_type_in(field.ty, &context);
            flat.push(FlatField {
                owner: None,
                plain: plain.clone(),
                storage: plain,
                ty,
                mutable: field.mutable,
                default: field
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source)),
            });
        }
        self.apply_overrides(declaration, &mut flat);
        self.qualify_collisions(&mut flat);
        flat
    }

    /// Copies one parent's fields into the flattened list, skipping a slot the
    /// list already carries — a diamond inherits one copy of the shared base,
    /// not two.
    fn inherit_fields(&mut self, parent: StructId, flat: &mut Vec<FlatField>) {
        let Some(def) = self.program.types.structs().get(parent) else {
            return;
        };
        let inherited: Vec<(String, Type, bool)> = def
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.ty, field.mutable))
            .collect();
        // A parent that is itself a class already recorded where each slot came
        // from, including any qualification it applied; a struct parent owns
        // every slot under its written name.
        let origins: Vec<(StructId, String)> = match self.classes.get(&parent) {
            Some(info) => info.slot_origin.clone(),
            None => inherited
                .iter()
                .map(|(name, _, _)| (parent, name.clone()))
                .collect(),
        };
        for (slot, (storage, ty, mutable)) in inherited.into_iter().enumerate() {
            let (owner, plain) = match origins.get(slot) {
                Some(origin) => origin.clone(),
                None => (parent, storage.clone()),
            };
            if flat
                .iter()
                .any(|existing| existing.owner == Some(owner) && existing.plain == plain)
            {
                continue;
            }
            let default = self.field_default(parent, slot as u32);
            flat.push(FlatField {
                owner: Some(owner),
                plain,
                storage,
                ty,
                mutable,
                default,
            });
        }
    }

    /// Applies `override let name = value`, replacing the inherited slot's
    /// default.
    fn apply_overrides(&mut self, declaration: &ClassDecl, flat: &mut [FlatField]) {
        for entry in &declaration.overrides {
            let name = self.interner.resolve(entry.name).to_owned();
            let matches: Vec<usize> = flat
                .iter()
                .enumerate()
                .filter(|(_, field)| field.plain == name)
                .map(|(index, _)| index)
                .collect();
            match matches.as_slice() {
                [] => self.emit(
                    entry.name_span,
                    "KSEM072",
                    format!("`{name}` overrides no inherited field"),
                ),
                [only] => {
                    flat[*only].default = Some(FieldDefault::new(entry.default, self.source));
                }
                _ => self.emit(
                    entry.name_span,
                    "KSEM068",
                    format!(
                        "`{name}` is inherited from several parents, so this override is ambiguous"
                    ),
                ),
            }
        }
    }

    /// Stores every colliding name qualified (`ClsAlpha.v`), so two parents'
    /// same-named fields stay two distinct slots.
    ///
    /// `.` cannot appear in an identifier, so a qualified storage name can
    /// never collide with one a user wrote.
    fn qualify_collisions(&mut self, flat: &mut [FlatField]) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for field in flat.iter() {
            *counts.entry(field.plain.clone()).or_default() += 1;
        }
        for field in flat.iter_mut() {
            if counts.get(&field.plain).copied().unwrap_or(0) <= 1 {
                continue;
            }
            let Some(owner) = field.owner else {
                continue;
            };
            let owner = self.program.types.type_name(Type::Struct(owner));
            field.storage = format!("{owner}.{}", field.plain);
        }
    }

    /// Records how each bare field name resolves, now that slots are numbered.
    fn record_bare_fields(&mut self, info: &mut ClassInfo) {
        let mut by_name: HashMap<&str, Vec<(StructId, u32)>> = HashMap::new();
        for (slot, (owner, plain)) in info.slot_origin.iter().enumerate() {
            by_name
                .entry(plain.as_str())
                .or_default()
                .push((*owner, slot as u32));
        }
        let resolved: Vec<(String, Member<u32>)> = by_name
            .into_iter()
            .map(|(name, slots)| {
                let member = match slots.as_slice() {
                    [(_, slot)] => Member::One(*slot),
                    several => Member::Ambiguous(several.iter().map(|(owner, _)| *owner).collect()),
                };
                (name.to_owned(), member)
            })
            .collect();
        info.bare_fields = resolved.into_iter().collect();
    }

    /// Records which ancestor's body each method name names.
    fn resolve_methods(&mut self, own: StructId, parents: &[StructId], info: &mut ClassInfo) {
        for parent in parents {
            for pair in self.methods_of(*parent) {
                info.qualified_methods.insert(pair);
            }
        }
        for method in self.own_methods.get(&own).into_iter().flatten() {
            info.qualified_methods.insert((own, method.name.clone()));
        }
        // A bare name resolves to the definition no other definition is more
        // derived than. Two unrelated parents both defining it leaves two, and
        // the name is ambiguous until someone qualifies it.
        let mut by_name: HashMap<String, Vec<StructId>> = HashMap::new();
        for (owner, name) in &info.qualified_methods {
            by_name.entry(name.clone()).or_default().push(*owner);
        }
        for (name, owners) in by_name {
            let surviving: Vec<StructId> = owners
                .iter()
                .copied()
                .filter(|candidate| {
                    !owners.iter().any(|other| {
                        other != candidate
                            && self.inherits_from(*other, *candidate, own, &info.ancestors)
                    })
                })
                .collect();
            let member = match surviving.as_slice() {
                [only] => Member::One(*only),
                _ => Member::Ambiguous(surviving),
            };
            info.bare_methods.insert(name, member);
        }
    }

    /// Whether `descendant` inherits from `ancestor`.
    ///
    /// `own` is the class being declared, whose ancestors are not in the table
    /// yet, so they are passed alongside.
    fn inherits_from(
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

    /// Every `(owner, method name)` pair visible on `id`.
    fn methods_of(&self, id: StructId) -> Vec<(StructId, String)> {
        match self.classes.get(&id) {
            Some(info) => info.qualified_methods.iter().cloned().collect(),
            None => self
                .own_methods
                .get(&id)
                .into_iter()
                .flatten()
                .map(|method| (id, method.name.clone()))
                .collect(),
        }
    }

    /// Reports an `override` that overrides nothing, and one whose parameter
    /// count does not match what it overrides.
    fn check_overrides(&mut self, declaration: &ClassDecl, own: StructId) {
        let Some(info) = self.classes.get(&own).cloned() else {
            return;
        };
        for method in &declaration.methods {
            if !method.is_override {
                continue;
            }
            let name = self.interner.resolve(method.function.name).to_owned();
            let base = info
                .qualified_methods
                .iter()
                .find(|(owner, method_name)| *owner != own && *method_name == name)
                .map(|(owner, _)| *owner);
            let Some(base) = base else {
                self.emit(
                    method.function.name_span,
                    "KSEM073",
                    format!("`{name}` overrides no inherited method"),
                );
                continue;
            };
            let overridden = self
                .own_methods
                .get(&base)
                .into_iter()
                .flatten()
                .find(|candidate| candidate.name == name)
                .map(|candidate| candidate.arity);
            if overridden.is_some_and(|arity| arity != method.function.params.len()) {
                let base_name = self.program.types.type_name(Type::Struct(base));
                self.emit(
                    method.function.name_span,
                    "KSEM066",
                    format!("`{name}` must match the signature it overrides from `{base_name}`"),
                );
            }
        }
    }
}

impl<'a> Analyzer<'a> {
    /// Registers one callable per `(class, ancestor, method)` triple.
    ///
    /// This is the monomorphization: an inherited body is registered again for
    /// each class that inherits it, with the receiver set to *that* class. The
    /// body is shared, the receiver type is not — which is what makes `self.m()`
    /// inside an inherited method resolve to the concrete class's `m` on every
    /// backend, with no dispatch at run time.
    pub(crate) fn class_callables(
        &self,
        declaration: &'a ClassDecl,
        source: SourceId,
        callables: &mut Vec<crate::analyze::Callable<'a>>,
    ) {
        let name = self.interner.resolve(declaration.name);
        // The class's own struct, like `construct_callables`: this registers
        // what the declaration provides rather than resolving a name someone
        // wrote, so it is not gated.
        let Some(id) = self.program.types.structs().lookup(name) else {
            // The class was not declared — a cycle or a duplicate name, already
            // reported. Registering its methods would give them no receiver.
            return;
        };
        let Some(info) = self.classes.get(&id) else {
            return;
        };
        for (owner, method) in &info.qualified_methods {
            let Some((function, origin_source)) = self.method_ast(*owner, method) else {
                continue;
            };
            callables.push(crate::analyze::Callable {
                receiver: Some(id),
                origin: Some(*owner),
                function,
                source: if *owner == id { source } else { origin_source },
            });
        }
    }

    /// The declaration of `owner`'s method `name`, and the file it is written
    /// in.
    ///
    /// A method's body resolves qualified names against the imports of the file
    /// it was *written* in, not the file the inheriting class was written in —
    /// so the origin's source travels with the body.
    fn method_ast(
        &self,
        owner: StructId,
        name: &str,
    ) -> Option<(&'a kira_syntax_model::ast::Function, SourceId)> {
        let owner_name = self.program.types.type_name(Type::Struct(owner));
        self.tree.items_with_source().find_map(|(source, item)| {
            let candidate = match item {
                Item::Struct(def) if self.interner.resolve(def.name) == owner_name => def
                    .methods
                    .iter()
                    .find(|method| self.interner.resolve(method.name) == name),
                Item::Class(def) if self.interner.resolve(def.name) == owner_name => def
                    .methods
                    .iter()
                    .map(|method| &method.function)
                    .find(|method| self.interner.resolve(method.name) == name),
                _ => None,
            }?;
            Some((candidate, source))
        })
    }
}
