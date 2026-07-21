//! The construct declaration family, made executable.
//!
//! The oracle documents the construct family as validate-only: "construct-backed
//! declarations do not execute yet". Here they execute. A construct-backed
//! declaration `Family Name(params) { members }` is a typed factory, so it is
//! compiled as a class-shaped struct:
//!
//! - the declared **params** become the struct's stored fields, filled by the
//!   construction call `Name(args)` (positional or by parameter name);
//! - each **computed member** `let node: T { block }` becomes a zero-argument
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

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{CallArg, ConstructDecl, ConstructKind, Item};

use crate::analyze::{Analyzer, FnCtx};

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
        for field in &declaration.fields {
            let field_name = self.interner.resolve(field.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, field.name_span);
            let ty = self.resolve_type_ref(field.ty);
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: false,
            });
            defaults.push(field.default);
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
            },
        );
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
    fn refuse_deferred(&mut self, declaration: &ConstructDecl) {
        for deferred in &declaration.deferred {
            self.emit(
                deferred.span,
                "KSEM203",
                format!(
                    "{} is not executable yet in a construct; the executable slice supports \
                     `@Required let`, `let name: T = default`, computed `let node: T {{ … }}` \
                     bridges, and `function` members",
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

    /// Type-checks `Name(args)`: a construct-backed declaration's construction.
    ///
    /// The params fill the leading fields — positionally or by parameter name —
    /// and any remaining field takes its declared default. The result is the
    /// same [`HirExpr::StructNew`] a struct literal or a class constructor
    /// produces, so downstream sees a fully initialized struct.
    pub(crate) fn analyze_construct_new(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
        let name = self.program.types.type_name(Type::Struct(id));
        let param_count = self.construct_param_count(id);
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.len())
            .unwrap_or_default();
        // The param name and type for each of the leading slots.
        let param_slots: Vec<(String, Type)> = (0..param_count)
            .filter_map(|slot| {
                self.program
                    .types
                    .structs()
                    .get(id)
                    .and_then(|def| def.field(slot as u32))
                    .map(|field| (field.name.clone(), field.ty))
            })
            .collect();

        let mut initializers: Vec<Option<HirExprId>> = vec![None; field_count];
        let mut next_positional = 0usize;
        for arg in args {
            let value = self.analyze_expr(ctx, arg.value);
            let slot = match arg.label {
                Some(label) => {
                    let label = self.interner.resolve(label).to_owned();
                    match param_slots.iter().position(|(name, _)| *name == label) {
                        Some(slot) => slot,
                        None => {
                            self.emit(
                                arg.label_span.unwrap_or(span),
                                "KSEM204",
                                format!("`{name}` has no construction input named `{label}`"),
                            );
                            continue;
                        }
                    }
                }
                None => {
                    let slot = next_positional;
                    next_positional += 1;
                    if slot >= param_count {
                        self.emit(
                            span,
                            "KSEM205",
                            format!(
                                "`{name}` takes {param_count} construction input(s), found more"
                            ),
                        );
                        continue;
                    }
                    slot
                }
            };
            if initializers[slot].is_some() {
                let (field, _) = &param_slots[slot];
                self.emit(
                    span,
                    "KSEM206",
                    format!("construction input `{field}` of `{name}` is set more than once"),
                );
            }
            let expected = param_slots[slot].1;
            let actual = self.program.expr(value).type_of();
            if !actual.assignable_to(expected) {
                let (field, _) = &param_slots[slot];
                self.emit(
                    span,
                    "KSEM207",
                    format!(
                        "construction input `{field}` of `{name}` expects `{}`, found `{}`",
                        self.type_name(expected),
                        self.type_name(actual)
                    ),
                );
            }
            initializers[slot] = Some(value);
        }

        // Every slot is filled: a param from its argument, and each remaining
        // slot — an unset param or an own field — from its declared default.
        let mut slot = 0u32;
        while (slot as usize) < field_count {
            let index = slot as usize;
            slot += 1;
            if initializers[index].is_some() {
                continue;
            }
            let filled = match self.field_default(id, index as u32) {
                Some(default) => self.analyze_expr(ctx, default),
                None => {
                    let field = self
                        .program
                        .types
                        .structs()
                        .get(id)
                        .and_then(|def| def.field(index as u32))
                        .map(|field| field.name.clone())
                        .unwrap_or_default();
                    self.emit(
                        span,
                        "KSEM208",
                        format!("construction of `{name}` is missing input `{field}`"),
                    );
                    self.program.exprs.alloc(HirExpr::Error)
                }
            };
            initializers[index] = Some(filled);
        }
        let fields: Vec<HirExprId> = initializers
            .into_iter()
            .map(|value| value.unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error)))
            .collect();
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }

    /// Type-checks `value.node`: reading a construct's computed bridge member.
    ///
    /// The read runs the member, so it lowers to a call of the zero-argument
    /// method the member became, with `value` as the receiver.
    pub(crate) fn analyze_construct_bridge_read(
        &mut self,
        ctx: &mut FnCtx,
        base: HirExprId,
        id: StructId,
        member: &str,
        span: Span,
    ) -> HirExprId {
        let method = format!(
            "{}.{member}",
            self.program.types.type_name(Type::Struct(id))
        );
        self.analyze_user_call_from_syntax(ctx, &method, &[base], &[], span)
    }
}
