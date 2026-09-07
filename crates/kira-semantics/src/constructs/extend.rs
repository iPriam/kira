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

use kira_semantics_model::hir::{
    CallableSignature, FuncId, HirFunction, ParamSignature, ReceiverSignature, ThreadAffinity,
};
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
            let written = self.interner.resolve(declaration.name).to_owned();
            let family_name = self.visible_family_key(&written).unwrap_or(written);
            if !self.construct_families.contains_key(&family_name) {
                // A class is the other thing `extend` may name, and it is
                // handled where a class's methods are collected rather than
                // here: classes have no ids yet at this point in analysis, and
                // a class method is an ordinary method rather than a modifier.
                if self.extends_a_class(&family_name) {
                    continue;
                }
                self.emit(
                    declaration.name_span,
                    "KSEM238",
                    format!("`extend` names no construct family or class `{family_name}`"),
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
        self.refuse_annotations_a_modifier_cannot_carry(method);
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
                    required: false,
                    result_declared: method.return_type.is_some(),
                    params: Vec::new(),
                    ownership: Vec::new(),
                    result: Type::Error,
                    uniform: true,
                    dispatcher: None,
                    defaults: Vec::new(),
                },
            );
        }
    }

    /// Refuses the annotations a modifier has nowhere to carry.
    ///
    /// `@Runtime` and `@Native` reach the synthesized body and decide which half
    /// of a hybrid build it lands in. The rest have no answer here — a modifier
    /// is reached through the family, not through an entrypoint or an export
    /// boundary — and an annotation that reaches a body which ignores it is a
    /// silent lie about what was compiled, so each is named where it was
    /// written.
    fn refuse_annotations_a_modifier_cannot_carry(&mut self, method: &Function) {
        if method.is_main || method.is_main_thread_lifecycle {
            self.emit(
                method.name_span,
                "KSEM258",
                "an entrypoint annotation cannot decorate an `extend` modifier: a modifier is \
                 called on a family value, so there is nothing for the operating \
                 system to start"
                    .to_owned(),
            );
        }
        if let Some(mark) = method.export {
            self.emit(
                mark.span,
                "KSEM259",
                "`@Export` cannot annotate an `extend` modifier: a library \
                 exports free functions, and a modifier's receiver is the family \
                 value. Wrap it in an exported function."
                    .to_owned(),
            );
        }
        if let Some(mark) = &method.foreign {
            self.emit(
                mark.span,
                "KSEM260",
                "`@FFI.Extern` cannot annotate an `extend` modifier: a foreign \
                 function is a C symbol with no Kira body, and a modifier is the \
                 body it was written as"
                    .to_owned(),
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
        let (enum_id, _) = *self.constructs.get(&struct_id)?.families.first()?;
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
            if !self.synth_needs_body(body) {
                continue;
            }
            let function = self.build_extend_body(enum_id, &family, &method);
            self.fill_synth(body, function);
        }
    }

    fn build_extend_body(&mut self, enum_id: EnumId, family: &str, method: &str) -> HirFunction {
        // Snapshot the modifier's resolved shape and syntax before analyzing its
        // body: the immutable read of `construct_families` cannot overlap the
        // `&mut self` the body analysis needs.
        let Some((function, source, params, ownership, result, mutates_self)) = self
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
                    entry
                        .function
                        .receiver
                        .is_some_and(|receiver| receiver.mutable),
                )
            })
        else {
            return self.empty_extend_body();
        };

        self.source = source;
        self.current_execution = function.execution;
        let mut ctx = FnCtx::new(result);
        // Local 0 is the receiver `self`, the family value. A read-only
        // modifier lends it; a `borrow mut self` modifier receives the family
        // storage so its updated enum reaches the caller.
        ctx.declare_param(
            "self",
            Type::Enum(enum_id),
            mutates_self,
            if mutates_self {
                OwnershipMode::BorrowMut
            } else {
                OwnershipMode::BorrowRead
            },
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
        let signature = CallableSignature {
            receiver: Some(ReceiverSignature {
                ty: Type::Enum(enum_id),
                mutable: mutates_self,
            }),
            params: function
                .params
                .iter()
                .zip(params.iter())
                .zip(ownership.iter())
                .map(|((param, &ty), &mode)| ParamSignature {
                    label: self.interner.resolve(param.name).to_owned(),
                    ty,
                    ownership: mode,
                    has_default: param.default.is_some(),
                })
                .collect(),
            result,
            is_async: function.is_async,
            affinity: ThreadAffinity::Any,
            execution: function.execution,
        };
        HirFunction {
            name: format!("Any {family}.{method}"),
            param_count: 1 + function.params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_main_thread: false,
            is_async: false,
            // The engine the modifier was written to run on. A modifier's body
            // is synthesized, but it is the body the author wrote, so `@Native`
            // on it means what it means anywhere else: a hybrid build puts this
            // function in the native half. Hardcoding `Inherited` here is what
            // made the annotation vanish between the source and the split.
            execution: function.execution,
            mutates_self,
            name_span: function.name_span,
            signature,
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
            is_main_thread: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: kira_source::Span::new(0, 0),
            signature: CallableSignature::synthesized(&[], Type::Void),
        }
    }

    /// Whether `name` is a class this program declares.
    ///
    /// Asked of the tree rather than of the type table because classes have no
    /// ids yet when `extend` blocks are read: the header pass runs later, and it
    /// has to, since a class field may name a construct-backed type.
    fn extends_a_class(&self, name: &str) -> bool {
        self.tree.items_with_source().any(|(_, item)| {
            matches!(item, Item::Class(declaration)
                if self.interner.resolve(declaration.name) == name)
        })
    }

    fn extend_declarations(&self) -> Vec<(SourceId, &'a ExtendDecl)> {
        let tree: &'a kira_syntax_model::SyntaxTree = self.tree;
        tree.items_with_source()
            .filter_map(|(source, item)| match item {
                // `extend T: Trait { … }` on a *type* is an impl block, not a
                // modifier block: its members are the trait's members for that
                // one type, and they are registered as its methods rather than
                // as a family's chainable surface. See `crate::traits`.
                //
                // On a *family* the two coincide. A family cannot present a
                // member itself — it is a template — so the way it answers a
                // trait it claims is by carrying the member for every
                // declaration backed by it, which is exactly what a modifier
                // is.
                Item::Extend(declaration)
                    if declaration.conforms.is_none()
                        || self
                            .construct_families
                            .contains_key(self.interner.resolve(declaration.name)) =>
                {
                    Some((source, declaration))
                }
                _ => None,
            })
            .collect()
    }
}
