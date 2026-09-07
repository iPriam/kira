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
//! Subtyping is admitted, and the per-class copy is what makes it safe rather
//! than what it costs. A subclass may be passed where a parent is declared, and
//! the callee is registered again with that parameter typed as the subclass —
//! so the argument reaches a body where it is statically the concrete class and
//! an override still wins with nothing to dispatch. See
//! `Analyzer::specialize_callables`.
//!
//! The layout is free: a class flattens its parents' fields first, so a
//! subclass already has the parent's prefix and a position expecting the parent
//! reads exactly the slots it means to.

mod exprs;
mod fields;
mod parents;

use std::collections::{BTreeSet, HashMap};

use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::{ClassDecl, Item};

use crate::analyze::{Analyzer, FieldDefault};
use crate::types::NameContext;

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

/// One method a type declares itself, by the key it is known under.
///
/// The key is the name together with what it takes — see
/// [`Analyzer::member_key`] — rather than the name alone, because a name may be
/// **overloaded**: `bump()` and `bump(step: Int)` are two methods, and a
/// subclass overriding one leaves the other alone.
///
/// [`Analyzer::member_key`]: crate::analyze::Analyzer
#[derive(Debug, Clone)]
pub(crate) struct OwnMethod {
    /// The member key.
    pub(crate) key: String,
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
    /// Member key to the ancestor whose body it names.
    ///
    /// Keyed by [`Analyzer::member_key`] rather than by name, so two overloads
    /// of one name are two entries and a subclass overriding one of them does
    /// not shadow the other.
    ///
    /// [`Analyzer::member_key`]: crate::analyze::Analyzer
    pub(crate) bare_methods: HashMap<String, Member<StructId>>,
    /// Every `(owner, member key)` pair this class can name.
    pub(crate) qualified_methods: BTreeSet<(StructId, String)>,
    /// Slots the constructor fills positionally: the fields with no default, in
    /// flattened order.
    pub(crate) required_slots: Vec<u32>,
}

/// One field of a class being flattened, before it becomes a [`FieldDef`].
pub(crate) struct FlatField {
    /// The type that declared it; `None` for a field this class declares
    /// itself, whose id does not exist until the class is declared.
    pub(crate) owner: Option<StructId>,
    /// The name as written, before any qualification.
    pub(crate) plain: String,
    /// The storage name in the flattened struct.
    pub(crate) storage: String,
    pub(crate) ty: Type,
    pub(crate) mutable: bool,
    pub(crate) default: Option<FieldDefault>,
}

impl<'a> Analyzer<'a> {
    /// Declares every class's *name* as an empty row in the struct table.
    ///
    /// Runs before [`collect_structs`](Analyzer::collect_structs) for the reason
    /// enum headers run before it: a struct field may name a class
    /// (`var currentEncoder: RenderEncoder`) and a class field may name a struct,
    /// so each kind needs the other's names to exist before either resolves a
    /// field. Splitting the id from the fields is what lets both be true at once.
    ///
    /// Declaration order, not inheritance order: a header needs no parent, and
    /// the ordering that does — flattening — happens in [`Self::fill_classes`].
    ///
    /// Returns the id minted for each declaration, or `None` where the name lost
    /// a collision and was reported here.
    pub(crate) fn declare_class_headers(&mut self) -> Vec<Option<StructId>> {
        let declarations = self.class_declarations();
        let mut ids = Vec::with_capacity(declarations.len());
        for (source, declaration) in &declarations {
            if !declaration.type_params.is_empty() {
                ids.push(None);
                continue;
            }
            self.source = *source;
            let name = self.interner.resolve(declaration.name).to_owned();
            // One type namespace: a name an enum already took is not available
            // to a class either. A widget and an enum sharing a name is the
            // shape this catches — the class wins every lookup, and the enum's
            // own uses start reporting that it is not an enum.
            if self.name_taken_by_enum(&name, declaration.name_span, "class") {
                ids.push(None);
                continue;
            }
            // Filed under the declaring package, like every other declaration:
            // two packages may each declare a class of the same name.
            let owner = self.imports.package_of(*source).map(str::to_owned);
            let id = self.program.types.structs_mut().declare_owned(
                owner.as_deref(),
                StructDef {
                    name: name.clone(),
                    fields: Vec::new(),
                    c_layout: false,
                    drop_glue: None,
                },
            );
            match id {
                Some(id) => {
                    let module = self.imports.module_of(*source).to_owned();
                    self.program.types.structs_mut().set_module(id, &module);
                    self.struct_sources.insert(id, *source);
                    // Kept in step with the table, which classes and structs
                    // share: `struct_defaults` is indexed by the ids it mints,
                    // and the real defaults land here in the fill pass.
                    self.struct_defaults.push(Vec::new());
                }
                None => self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("class `{name}` is already defined"),
                ),
            }
            ids.push(id);
        }
        ids
    }

    /// Fills every declared class: flattens its inherited fields into the row
    /// its header reserved, and records its methods.
    ///
    /// In dependency order among classes, so a parent is always flattened before
    /// its child — which is what `extends` means and the one thing headers could
    /// not do on their own.
    pub(crate) fn collect_classes(&mut self, headers: &[Option<StructId>]) {
        let declarations = self.class_declarations();
        for index in self.class_order(&declarations) {
            let (source, declaration) = declarations[index];
            let Some(Some(id)) = headers.get(index).copied() else {
                continue;
            };
            self.source = source;
            self.fill_class(id, declaration);
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

    /// Flattens one class into the row its header reserved.
    fn fill_class(&mut self, id: StructId, declaration: &ClassDecl) {
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
        self.program.types.structs_mut().set_fields(id, fields);
        // Written rather than pushed: the header pass reserved this slot when it
        // minted the id, which is what keeps `struct_defaults` indexed by the
        // ids the table mints — classes and structs share that table.
        if let Some(slot) = self.struct_defaults.get_mut(id.index() as usize) {
            *slot = defaults;
        }
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
        let mut own: Vec<OwnMethod> = declaration
            .methods
            .iter()
            .map(|method| OwnMethod {
                key: self.member_key(
                    self.interner.resolve(method.function.name),
                    &method.function.params,
                ),
            })
            .collect();
        own.extend(self.extended_methods(&name, &own));
        self.own_methods.insert(id, own);
        self.resolve_methods(id, &parents, &mut info);
        self.classes.insert(id, info);
        self.check_overrides(declaration, id);
    }

    /// The methods `extend <Class> { … }` blocks add, reporting any that
    /// collides with one the class body already declares.
    ///
    /// This is what lets a class be written in more than one file. A class is
    /// one declaration by construction — its fields are flattened into one row,
    /// and that row is minted once — so the *body* cannot be split. Its methods
    /// can: an `extend` block adds ordinary methods to an existing class, and
    /// they resolve, lower and dispatch exactly as ones written inside the
    /// braces do, because they join the same `own_methods` list before anything
    /// downstream reads it.
    ///
    /// `already` is what the class body declared. A collision is refused rather
    /// than resolved by order: two definitions of the same method, one of them
    /// in a file the reader may not have open, is a coin toss dressed up as a
    /// rule.
    fn extended_methods(&mut self, class_name: &str, already: &[OwnMethod]) -> Vec<OwnMethod> {
        let blocks: Vec<(SourceId, &kira_syntax_model::ast::ExtendDecl)> = self
            .tree
            .items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Extend(declaration)
                    if self.interner.resolve(declaration.name) == class_name =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect();
        let mut added: Vec<OwnMethod> = Vec::new();
        for (source, block) in blocks {
            self.source = source;
            for method in &block.methods {
                let name = self.interner.resolve(method.name).to_owned();
                let key = self.member_key(&name, &method.params);
                // An `extend` block may *overload* a method the class already
                // has — a different parameter list is a different method — but
                // it may not restate one, which would be two bodies for one
                // call with no rule saying which runs.
                if already
                    .iter()
                    .chain(added.iter())
                    .any(|seen| seen.key == key)
                {
                    self.emit(
                        method.name_span,
                        "KSEM257",
                        format!(
                            "`{class_name}` already declares a method `{name}` taking these \
                             parameters"
                        ),
                    );
                    continue;
                }
                added.push(OwnMethod { key });
            }
        }
        added
    }

    /// Records which ancestor's body each method name names.
    pub(crate) fn resolve_methods(
        &mut self,
        own: StructId,
        parents: &[StructId],
        info: &mut ClassInfo,
    ) {
        for parent in parents {
            for pair in self.methods_of(*parent) {
                info.qualified_methods.insert(pair);
            }
        }
        for method in self.own_methods.get(&own).into_iter().flatten() {
            info.qualified_methods.insert((own, method.key.clone()));
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

    /// Every `(owner, method name)` pair visible on `id`.
    fn methods_of(&self, id: StructId) -> Vec<(StructId, String)> {
        match self.classes.get(&id) {
            Some(info) => info.qualified_methods.iter().cloned().collect(),
            None => self
                .own_methods
                .get(&id)
                .into_iter()
                .flatten()
                .map(|method| (id, method.key.clone()))
                .collect(),
        }
    }

    /// Reports an `override` that overrides nothing, and one whose parameter
    /// count does not match what it overrides.
    pub(crate) fn check_overrides(&mut self, declaration: &ClassDecl, own: StructId) {
        let Some(info) = self.classes.get(&own).cloned() else {
            return;
        };
        for method in &declaration.methods {
            if !method.is_override {
                continue;
            }
            let name = self.interner.resolve(method.function.name).to_owned();
            let key = self.member_key(&name, &method.function.params);
            // An override replaces the inherited method it *matches*, which is
            // the one taking the same parameters. A name the ancestors overload
            // therefore keeps every overload the subclass did not restate.
            let base = info
                .qualified_methods
                .iter()
                .find(|(owner, member)| *owner != own && *member == key)
                .map(|(owner, _)| *owner);
            if base.is_some() {
                continue;
            }
            // Overriding a name the base declares under a different parameter
            // list is the likelier mistake, and saying so beats "overrides
            // nothing" when the name plainly exists.
            let mismatched = info
                .qualified_methods
                .iter()
                .filter(|(owner, _)| *owner != own)
                .find_map(|(owner, member)| {
                    member.starts_with(&format!("{name}(")).then_some(*owner)
                });
            match mismatched {
                Some(base) => {
                    let base_name = self.program.types.type_name(Type::Struct(base));
                    self.emit(
                        method.function.name_span,
                        "KSEM066",
                        format!(
                            "`{name}` must match the signature it overrides from `{base_name}`"
                        ),
                    );
                }
                None => self.emit(
                    method.function.name_span,
                    "KSEM073",
                    format!("`{name}` overrides no inherited method"),
                ),
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
        // wrote, so it looks up under the declaring package.
        let owner = self.imports.package_of(source);
        let Some(id) = self.program.types.structs().lookup_owned(owner, name) else {
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
                receiver: Some(Type::Struct(id)),
                origin: Some(*owner),
                specialize: Vec::new(),
                initializes: None,
                function,
                // Spans are offsets in the file containing the method body.
                source: origin_source,
                type_bindings: self.generic_method_bindings(*owner),
            });
        }
    }

    /// The declaration of `owner`'s method with member key `key`, and the file
    /// it is written in.
    ///
    /// A method's body resolves qualified names against the imports of the file
    /// it was *written* in, not the file the inheriting class was written in —
    /// so the origin's source travels with the body.
    ///
    /// Matched on the key rather than the name, because a name may be
    /// overloaded and the two declarations are two different bodies.
    ///
    /// The candidate must also be written in the package that *owns* the
    /// receiver struct: two packages may each declare a class of one name, and
    /// the first tree-order match by name alone would compile the other
    /// package's body against this package's field layout.
    pub(crate) fn method_ast(
        &self,
        owner: StructId,
        key: &str,
    ) -> Option<(&'a kira_syntax_model::ast::Function, SourceId)> {
        let owner_name = self.program.types.type_name(Type::Struct(owner));
        // A template key is package-qualified; the declaration's name is
        // what follows the qualifier.
        let declaration_name = self
            .generic_instance_templates
            .get(&owner)
            .map_or(owner_name.as_str(), |key| {
                key.rsplit("::").next().unwrap_or(key)
            });
        let owner_package = self.program.types.structs().owner_of(owner);
        self.tree.items_with_source().find_map(|(source, item)| {
            if self.imports.package_of(source) != owner_package {
                return None;
            }
            let matches = |function: &&'a kira_syntax_model::ast::Function| {
                self.member_key(self.interner.resolve(function.name), &function.params) == key
            };
            let candidate = match item {
                Item::Struct(def) if self.interner.resolve(def.name) == declaration_name => {
                    def.methods.iter().find(matches)
                }
                Item::Class(def) if self.interner.resolve(def.name) == declaration_name => def
                    .methods
                    .iter()
                    .map(|method| &method.function)
                    .find(matches),
                // A method an `extend <Class>` block added, which is how a class
                // is written across more than one file. Found here rather than
                // registered separately so it is the same lookup: everything
                // downstream asks this one question to reach a method's body.
                Item::Extend(def) if self.interner.resolve(def.name) == declaration_name => {
                    def.methods.iter().find(matches)
                }
                _ => None,
            }?;
            Some((candidate, source))
        })
    }
}
