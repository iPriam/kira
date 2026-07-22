//! `extend Family { function … }` fluent modifiers.
//!
//! A modifier is a family-level method with **one shared body** whose receiver
//! is the family value (`Any Family`), distinct from the per-variant methods a
//! `construct` family declares. It returns the family type and wraps `self` — so
//! `text.padding(8).background(fill)` chains, each modifier constructing a layer
//! that holds the receiver.
//!
//! The oracle documents `extend` as validate-only. Here each modifier lowers to
//! one synthesized [`HirFunction`] taking `self: Any Family` as local 0, so a
//! call — on the family value or on a concrete widget upcast into it — is an
//! ordinary direct call and runs byte-identically on every backend. No new IR,
//! opcode, or serialized shape is introduced.

use kira_semantics_model::hir::{FuncId, HirFunction};
use kira_semantics_model::{EnumId, OwnershipMode, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::{ExtendDecl, Function, Item};

use super::ConstructFamilyMethod;
use crate::analyze::{Analyzer, FnCtx};

impl<'a> Analyzer<'a> {
    /// Registers every `extend Family { … }` modifier against its family.
    ///
    /// Runs after family headers exist so the family is known, and before family
    /// method signatures are resolved so a modifier's types are resolved with
    /// the rest. A modifier whose name is already taken by the family is refused
    /// rather than shadowing the declared method.
    pub(crate) fn collect_extend_blocks(&mut self) {
        for (source, declaration) in self.extend_declarations() {
            self.source = source;
            let family_name = self.interner.resolve(declaration.name).to_owned();
            if !self.construct_families.contains_key(&family_name) {
                self.emit(
                    declaration.name_span,
                    "KSEM238",
                    format!("`extend` names unknown construct family `{family_name}`"),
                );
                continue;
            }
            for method in &declaration.methods {
                self.register_extend_method(&family_name, method, source);
            }
        }
    }

    /// Registers one modifier, reporting a name that collides with an existing
    /// family method.
    fn register_extend_method(
        &mut self,
        family_name: &str,
        method: &'a Function,
        source: SourceId,
    ) {
        let method_name = self.interner.resolve(method.name).to_owned();
        let taken = self
            .construct_families
            .get(family_name)
            .is_some_and(|info| info.methods.contains_key(&method_name));
        if taken {
            self.emit(
                method.name_span,
                "KSEM239",
                format!(
                    "modifier `{method_name}` is already a method of construct family \
                     `{family_name}`"
                ),
            );
            return;
        }
        if let Some(info) = self.construct_families.get_mut(family_name) {
            info.methods.insert(
                method_name,
                ConstructFamilyMethod {
                    function: method,
                    source,
                    computed: false,
                    params: Vec::new(),
                    param_names: Vec::new(),
                    ownership: Vec::new(),
                    result: Type::Error,
                    uniform: true,
                    dispatcher: None,
                },
            );
        }
    }

    /// Reserves the synthesized-function id every modifier body lowers into.
    ///
    /// Reserved up front — not lazily at a call site like a dispatcher — so an
    /// uncalled modifier is still type-checked and lowered, matching the
    /// oracle's "modifier bodies are validated" guarantee. Runs after
    /// `synth_base` is fixed.
    pub(crate) fn reserve_extend_bodies(&mut self) {
        let rows: Vec<(String, String)> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.methods
                    .iter()
                    .filter(|(_, method)| method.uniform)
                    .map(move |(name, _)| (family.clone(), name.clone()))
            })
            .collect();
        for (family, method) in rows {
            let id = self.reserve_synth();
            if let Some(entry) = self
                .construct_families
                .get_mut(&family)
                .and_then(|info| info.methods.get_mut(&method))
            {
                entry.dispatcher = Some(id);
            }
        }
    }

    /// The family a uniform modifier `name` belongs to, when a concrete backed
    /// struct's own methods do not answer the call.
    ///
    /// This is what lets `Text(…).padding(8)` resolve: the concrete receiver is
    /// upcast into the family and the modifier is called on it.
    pub(crate) fn family_uniform_method(
        &self,
        struct_id: kira_semantics_model::StructId,
        name: &str,
    ) -> Option<EnumId> {
        let (enum_id, _) = self.constructs.get(&struct_id)?.family?;
        let family = self.construct_family_names.get(&enum_id)?;
        let method = self.construct_families.get(family)?.methods.get(name)?;
        method.uniform.then_some(enum_id)
    }

    /// Whether a family method is a uniform `extend` modifier called directly.
    pub(crate) fn family_method_is_uniform(&self, family_id: EnumId, method: &str) -> bool {
        self.construct_family_names
            .get(&family_id)
            .and_then(|family| self.construct_families.get(family))
            .and_then(|info| info.methods.get(method))
            .is_some_and(|method| method.uniform)
    }

    /// The single body a uniform modifier is called through.
    pub(crate) fn family_uniform_body(&self, family_id: EnumId, method: &str) -> Option<FuncId> {
        self.construct_family_names
            .get(&family_id)
            .and_then(|family| self.construct_families.get(family))
            .and_then(|info| info.methods.get(method))
            .filter(|method| method.uniform)
            .and_then(|method| method.dispatcher)
    }

    /// Fills every reserved modifier body.
    ///
    /// Runs alongside the dispatcher build, before synthesized functions are
    /// appended. Each body binds `self` to the family value as local 0, then the
    /// declared parameters, and analyzes the written block.
    pub(crate) fn build_extend_methods(&mut self) {
        let rows: Vec<(EnumId, String, String, FuncId)> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                let enum_id = info.enum_id;
                info.methods.iter().filter_map(move |(name, method)| {
                    method
                        .dispatcher
                        .filter(|_| method.uniform)
                        .map(|body| (enum_id, family.clone(), name.clone(), body))
                })
            })
            .collect();
        for (enum_id, family, method, body) in rows {
            let function = self.build_extend_body(enum_id, &family, &method);
            self.fill_synth(body, function);
        }
    }

    fn build_extend_body(&mut self, enum_id: EnumId, family: &str, method: &str) -> HirFunction {
        // Snapshot the modifier's resolved shape and syntax before analyzing its
        // body: the immutable read of `construct_families` cannot overlap the
        // `&mut self` the body analysis needs.
        let Some((function, source, params, ownership, result)) = self
            .construct_families
            .get(family)
            .and_then(|info| info.methods.get(method))
            .map(|entry| {
                (
                    entry.function,
                    entry.source,
                    entry.params.clone(),
                    entry.ownership.clone(),
                    entry.result,
                )
            })
        else {
            return self.empty_extend_body();
        };

        self.source = source;
        self.current_execution = function.execution;
        let mut ctx = FnCtx::new(result);
        // Local 0 is the receiver `self`, the family value. It is read as a
        // whole value (wrapped into a layer's child slot), never mutated, so it
        // is an immutable borrow — the family enum has no fields to write.
        ctx.declare_param(
            "self",
            Type::Enum(enum_id),
            false,
            OwnershipMode::BorrowRead,
        );
        for (index, param) in function.params.iter().enumerate() {
            let ty = params.get(index).copied().unwrap_or(Type::Error);
            let mode = ownership
                .get(index)
                .copied()
                .unwrap_or(OwnershipMode::Owned);
            let mutable = mode == OwnershipMode::BorrowMut;
            let name = self.interner.resolve(param.name).to_owned();
            let local = ctx.declare_param(&name, ty, mutable, mode);
            ctx.note_binding_span(local, param.name_span);
        }
        let body = self.analyze_block(&mut ctx, &function.body);
        if result != Type::Void && result != Type::Error && !self.body_definitely_returns(&body) {
            self.emit(
                function.name_span,
                "KSEM033",
                format!("modifier `Any {family}.{method}` may finish without returning a value"),
            );
        }
        HirFunction {
            name: format!("Any {family}.{method}"),
            param_count: 1 + function.params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: function.name_span,
        }
    }

    fn empty_extend_body(&self) -> HirFunction {
        HirFunction {
            name: "<unreachable extend modifier>".to_owned(),
            param_count: 0,
            return_type: Type::Void,
            locals: Vec::new(),
            body: Vec::new(),
            is_main: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: kira_source::Span::new(0, 0),
        }
    }

    fn extend_declarations(&self) -> Vec<(SourceId, &'a ExtendDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                Item::Extend(declaration) => Some((source, declaration)),
                _ => None,
            })
            .collect()
    }
}
