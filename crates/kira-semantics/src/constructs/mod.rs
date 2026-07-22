//! The construct declaration family, made executable.
//!
//! The oracle documents the construct family as validate-only: "construct-backed
//! declarations do not execute yet". Here they execute. A construct-backed
//! declaration `Family Name(params) { members }` is a typed factory, so it is
//! compiled as a class-shaped struct:
//!
//! - the declared **params** become the struct's stored fields, filled by the
//!   construction call `Name(args)` (positional or by parameter name);
//! - each **computed member** `let node: Any { block }` becomes a zero-argument
//!   method whose receiver is the declaration, so the block's bare names read
//!   the declaration's fields — and reading `value.node` runs that method;
//! - each **`function` member** becomes an ordinary method.
//!
//! Nothing below semantics learns constructs exist: construction lowers to the
//! same [`HirExpr::StructNew`](kira_semantics_model::hir::HirExpr::StructNew) a
//! struct literal does, a bridge read lowers to a method call, and every backend
//! runs the result unchanged. That is what makes a construct-backed declaration
//! run byte-identically on the vm, llvm, and hybrid backends.
//!
//! Inheritance (`extends`/`requires`), child slots (`@Content`, `some X`),
//! fluent modifiers (`extend C { }`), and consuming methods (`@Consuming`) are
//! not executable yet; each is refused at its declaration with a precise typed
//! diagnostic rather than dropped.

use std::collections::{HashMap, HashSet};

use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{BODY_SHORTHAND_LABEL, ConstructDecl, ConstructKind, Item};

use crate::analyze::{Analyzer, FieldDefault};

mod construction;

/// Everything analysis remembers about one construct-backed declaration beyond
/// its struct shape.
#[derive(Debug, Clone, Default)]
pub(crate) struct ConstructInfo {
    /// The number of leading struct fields that are construction params.
    pub(crate) param_count: usize,
    /// The member names read as a property rather than a field: the computed
    /// bridge members (`node`), whether declared here or inherited from the
    /// family. A `value.node` read of one lowers to a method call.
    pub(crate) computed: HashSet<String>,
    /// The child slots this declaration takes, in declaration order: fields
    /// filled from a construction's trailing children rather than from an
    /// argument or a default.
    pub(crate) slots: Vec<ContentSlot>,
}

/// One child slot of a construct-backed declaration: a field filled by the
/// bare children of a construction's trailing `{ … }` block.
#[derive(Debug, Clone)]
pub(crate) struct ContentSlot {
    /// The slot field's index in the struct's fields.
    pub(crate) field_index: u32,
    /// The slot field's name (its channel name).
    pub(crate) name: String,
    /// Whether the slot holds an ordered list (`[some X]`) rather than exactly
    /// one child (`some X`).
    pub(crate) list: bool,
    /// The element type each child must satisfy (`X`).
    pub(crate) element_ty: Type,
    /// The slot field's stored type (`X`, or `[X]` for a list slot).
    pub(crate) field_ty: Type,
}

/// One family template's conformance surface.
struct FamilyInfo {
    /// The `@Required let` member names every backed declaration must satisfy.
    required: Vec<String>,
    /// The computed bridge member names (`node`) the family declares.
    bridges: Vec<String>,
}

impl<'a> Analyzer<'a> {
    /// Declares every construct-backed declaration as a struct and checks each
    /// against its family.
    ///
    /// Runs after structs and classes are collected, because a param or a
    /// computed member may name any of them, and before signatures are
    /// collected, because a backed declaration's methods take signature slots.
    pub(crate) fn collect_constructs(&mut self) {
        let families = self.family_infos();
        let backed = self.backed_declarations();
        for (source, declaration) in backed {
            self.source = source;
            self.declare_construct(declaration, &families);
        }
        // Family templates carry no runtime shape, but their not-yet-executable
        // clauses are still refused so the author is told, not ignored.
        for (source, declaration) in self.family_declarations() {
            self.source = source;
            self.refuse_deferred(declaration);
        }
    }

    /// Every family template's conformance surface, keyed by name.
    fn family_infos(&self) -> HashMap<String, FamilyInfo> {
        let mut families = HashMap::new();
        for (_, item) in self.tree.items_with_source() {
            let Item::Construct(declaration) = item else {
                continue;
            };
            if !matches!(declaration.kind, ConstructKind::Family) {
                continue;
            }
            let required = declaration
                .fields
                .iter()
                .filter(|field| field.required)
                .map(|field| self.interner.resolve(field.name).to_owned())
                .collect();
            let bridges = declaration
                .methods
                .iter()
                .filter(|method| method.computed)
                .map(|method| self.interner.resolve(method.function.name).to_owned())
                .collect();
            families.insert(
                self.interner.resolve(declaration.name).to_owned(),
                FamilyInfo { required, bridges },
            );
        }
        families
    }

    /// Every construct-backed declaration, with the file it was written in.
    fn backed_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Construct(declaration)
                    if matches!(declaration.kind, ConstructKind::Backed { .. }) =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect()
    }

    /// Every family template, with the file it was written in.
    fn family_declarations(&self) -> Vec<(SourceId, &'a ConstructDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Construct(declaration)
                    if matches!(declaration.kind, ConstructKind::Family) =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect()
    }

    /// Declares one backed declaration as a struct and checks its conformance.
    fn declare_construct(
        &mut self,
        declaration: &ConstructDecl,
        families: &HashMap<String, FamilyInfo>,
    ) {
        let ConstructKind::Backed {
            family,
            family_span,
            params,
        } = &declaration.kind
        else {
            return;
        };
        let name = self.interner.resolve(declaration.name).to_owned();
        let family_name = self.interner.resolve(*family).to_owned();

        // The stored fields: params first (filled by the construction call),
        // then any own `let` fields (filled by their defaults).
        let mut fields = Vec::new();
        let mut defaults = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for param in params {
            let field_name = self.interner.resolve(param.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, param.name_span);
            let ty = self.resolve_type_ref(param.ty);
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: false,
            });
            defaults.push(None);
        }
        let param_count = fields.len();
        let mut slots: Vec<ContentSlot> = Vec::new();
        for field in &declaration.fields {
            let field_name = self.interner.resolve(field.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, field.name_span);
            let field_index = fields.len() as u32;
            let ty = if field.slot {
                self.resolve_slot_field(field, field_index, &field_name, families, &mut slots)
            } else {
                self.resolve_type_ref(field.ty)
            };
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: false,
            });
            // A child slot is filled from the trailing children, never from a
            // default, so a default written on one is meaningless.
            let default = if field.slot {
                None
            } else {
                field
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source))
            };
            defaults.push(default);
        }
        // Computed and function members share the member namespace; a name that
        // collides with a field is a duplicate.
        let mut computed: HashSet<String> = HashSet::new();
        for method in &declaration.methods {
            let member = self.interner.resolve(method.function.name).to_owned();
            self.note_duplicate_member(&mut seen, &member, method.function.name_span);
            if method.computed {
                computed.insert(member);
            }
        }

        // Conformance against the family.
        match families.get(&family_name) {
            None => {
                self.emit(
                    *family_span,
                    "KSEM200",
                    format!("`{name}` is backed by unknown construct family `{family_name}`"),
                );
            }
            Some(info) => {
                // Terminal rule: a declaration that provides every family bridge
                // member itself discharges the family's required inputs — the
                // default bridge that would have read them is overridden.
                let terminal = info.bridges.iter().all(|bridge| computed.contains(bridge));
                if !terminal {
                    // A bridge the declaration did not override is inherited from
                    // the family, so it is a computed member here too.
                    for bridge in &info.bridges {
                        if !computed.contains(bridge) {
                            computed.insert(bridge.clone());
                        }
                    }
                    for required in &info.required {
                        if !seen.contains(required) {
                            self.emit(
                                declaration.name_span,
                                "KSEM201",
                                format!(
                                    "`{name}` does not provide required member `{required}` of \
                                     construct family `{family_name}`, and does not override the \
                                     family's bridge to discharge it"
                                ),
                            );
                        }
                    }
                }
            }
        }

        self.refuse_deferred(declaration);

        let Some(id) = self
            .program
            .types
            .structs_mut()
            .declare(StructDef { name, fields })
        else {
            self.emit(
                declaration.name_span,
                "KSEM004",
                format!(
                    "`{}` is already defined",
                    self.interner.resolve(declaration.name)
                ),
            );
            return;
        };
        self.struct_defaults.push(defaults);
        self.constructs.insert(
            id,
            ConstructInfo {
                param_count,
                computed,
                slots,
            },
        );
    }

    /// Resolves a child slot field's stored type and records it as an executable
    /// [`ContentSlot`] when its element type is concrete.
    ///
    /// A slot over a construct **family** (`some Widget`, `[some Widget]`) is
    /// the heterogeneous case — storing differently-shaped children under one
    /// family supertype needs the `Any Construct` composition the executable
    /// slice does not cover — so it is refused here and its field is given
    /// `Error` type, which keeps a later read of the field silent rather than
    /// cascading. A slot over a concrete type (a `struct`, a `class`, or another
    /// construct-backed declaration) is a real field and executes.
    fn resolve_slot_field(
        &mut self,
        field: &kira_syntax_model::ast::ConstructField,
        field_index: u32,
        field_name: &str,
        families: &HashMap<String, FamilyInfo>,
        slots: &mut Vec<ContentSlot>,
    ) -> Type {
        let (element_ref, list) = match self.tree.type_ref(field.ty) {
            kira_syntax_model::ast::TypeRef::Array { element, .. } => (*element, true),
            _ => (field.ty, false),
        };
        if let Some(name) = self.type_ref_head_name(element_ref)
            && families.contains_key(&name)
        {
            self.emit(
                field.name_span,
                "KSEM228",
                format!(
                    "child slot `{field_name}` holds the construct family `{name}`, whose \
                     children are differently-shaped; heterogeneous child composition — an \
                     `Any {name}` that dispatches at run time — is not executable yet, so a slot \
                     over a concrete type runs and this one is deferred"
                ),
            );
            return Type::Error;
        }
        let element_ty = self.resolve_type_ref(element_ref);
        let field_ty = if list {
            self.program.types.array_of(element_ty)
        } else {
            element_ty
        };
        slots.push(ContentSlot {
            field_index,
            name: field_name.to_owned(),
            list,
            element_ty,
            field_ty,
        });
        field_ty
    }

    /// The head type name a type reference names (`Widget` for `Widget`,
    /// `[Widget]`, or `Widget<Element>`), when it names one.
    fn type_ref_head_name(&self, id: kira_syntax_model::ast::TypeRefId) -> Option<String> {
        match self.tree.type_ref(id) {
            kira_syntax_model::ast::TypeRef::Named { name, .. }
            | kira_syntax_model::ast::TypeRef::Generic { name, .. } => {
                Some(self.interner.resolve(*name).to_owned())
            }
            kira_syntax_model::ast::TypeRef::Array { element, .. } => {
                self.type_ref_head_name(*element)
            }
            _ => None,
        }
    }

    /// Registers a backed declaration's methods as callables: every own member
    /// method (computed or `function`), plus each family bridge member it did
    /// not override, inherited so `value.node` still resolves.
    pub(crate) fn construct_callables(
        &self,
        declaration: &'a ConstructDecl,
        source: SourceId,
        callables: &mut Vec<crate::analyze::Callable<'a>>,
    ) {
        let ConstructKind::Backed { family, .. } = &declaration.kind else {
            return;
        };
        let name = self.interner.resolve(declaration.name);
        let Some(id) = self.program.types.structs().lookup(name) else {
            // Not declared — an unknown family or a duplicate name, already
            // reported. Registering its methods would give them no receiver.
            return;
        };
        // A backed declaration is only registered as a construct when its struct
        // was declared, so a missing entry means declaration failed.
        if !self.constructs.contains_key(&id) {
            return;
        }
        let mut own: HashSet<&str> = HashSet::new();
        for method in &declaration.methods {
            own.insert(self.interner.resolve(method.function.name));
            callables.push(crate::analyze::Callable {
                receiver: Some(id),
                origin: None,
                function: &method.function,
                source,
            });
        }
        // Inherit each family bridge the declaration did not override.
        let family_name = self.interner.resolve(*family);
        for (bridge, bridge_source) in self.family_bridges(family_name) {
            if own.contains(self.interner.resolve(bridge.function.name)) {
                continue;
            }
            callables.push(crate::analyze::Callable {
                receiver: Some(id),
                origin: None,
                function: &bridge.function,
                source: bridge_source,
            });
        }
    }

    /// The computed bridge members a family template declares, each with the
    /// file it was written in.
    fn family_bridges(
        &self,
        family_name: &str,
    ) -> Vec<(&'a kira_syntax_model::ast::ConstructMethod, SourceId)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        let mut bridges = Vec::new();
        for (source, item) in tree.items_with_source() {
            let Item::Construct(declaration) = item else {
                continue;
            };
            if !matches!(declaration.kind, ConstructKind::Family) {
                continue;
            }
            if self.interner.resolve(declaration.name) != family_name {
                continue;
            }
            for method in &declaration.methods {
                if method.computed {
                    bridges.push((method, source));
                }
            }
        }
        bridges
    }

    /// Records a member name, reporting a duplicate the second time it is seen.
    fn note_duplicate_member(&mut self, seen: &mut HashSet<String>, name: &str, span: Span) {
        if !seen.insert(name.to_owned()) {
            self.emit(
                span,
                "KSEM202",
                format!("construct member `{name}` is declared more than once"),
            );
        }
    }

    /// Refuses each not-yet-executable construct feature with a precise typed
    /// diagnostic — never silently, never as the generic parse-don't-crash node.
    ///
    /// A `body { … }` shorthand is separated out: its value is the construct
    /// family — the heterogeneous case — so it is the same deferral a
    /// family-typed child slot is (`KSEM228`), not a structural clause. That
    /// keeps `KSEM203` for what is genuinely structural (`extends`, `requires`,
    /// `@Consuming`, an unknown annotation).
    fn refuse_deferred(&mut self, declaration: &ConstructDecl) {
        for deferred in &declaration.deferred {
            if deferred.label == BODY_SHORTHAND_LABEL {
                self.emit(
                    deferred.span,
                    "KSEM228",
                    "a `body { … }` member yields a value of the construct family — the \
                     heterogeneous case; producing one needs the `Any Construct` dynamic \
                     dispatch that is not executable yet. Write a computed `let node: Any { … }` \
                     bridge over a concrete type, or a concrete `some X` child slot",
                );
                continue;
            }
            self.emit(
                deferred.span,
                "KSEM203",
                format!(
                    "{} is not executable yet in a construct; the executable slice supports \
                     `@Required let`, `let name: Any = default`, computed `let node: Any {{ … }}` \
                     bridges, `function` members, and `some X` / `[some X]` child slots over a \
                     concrete type",
                    deferred.label
                ),
            );
        }
    }

    /// The struct a construct-backed declaration named `name` was compiled to,
    /// when `name` is one.
    pub(crate) fn construct_backed_named(&self, name: &str) -> Option<StructId> {
        let id = self.program.types.structs().lookup(name)?;
        self.constructs.contains_key(&id).then_some(id)
    }

    /// Whether `name` is a computed bridge member of construct-backed `id`.
    pub(crate) fn construct_computed_member(&self, id: StructId, name: &str) -> bool {
        self.constructs
            .get(&id)
            .is_some_and(|info| info.computed.contains(name))
    }

    /// The number of leading fields of construct-backed `id` that are params.
    pub(crate) fn construct_param_count(&self, id: StructId) -> usize {
        self.constructs
            .get(&id)
            .map(|info| info.param_count)
            .unwrap_or_default()
    }
}
