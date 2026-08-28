//! Trait existentials: a trait's name used as a type.
//!
//! `let x: Hashable = …` holds "a value of some type that conforms to
//! `Hashable`", dispatched dynamically. The representation is the construct
//! family's, generalized rather than reinvented: analysis synthesizes one enum
//! over the types the conformance table answers for —
//!
//! ```text
//! some Hashable = Plate(Plate) | Circle(Circle) | …
//! ```
//!
//! — and a member call through the value lowers to the same per-variant tag
//! dispatcher a family call does, so both engines execute ordinary enum
//! projection, branching, and direct calls with no new wire surface.
//!
//! The enum is reserved the first time the trait's name resolves in a type
//! position and filled after every conformance is recorded, which is the same
//! two-phase shape the family enums follow: a reference earlier in the program
//! needs the id before the variants can be known.

use std::collections::BTreeMap;

use crate::analyze::{Analyzer, FnCtx};
use crate::constructs::ConstructVariant;
use crate::constructs::DispatchMethod;
use crate::place::PlacePurpose;
use kira_semantics_model::hir::{
    Callee, FuncId, HirExpr, HirExprId, HirFunction, HirStmt, HirWriteback,
};
use kira_semantics_model::{EnumId, Execution, OwnershipMode, Type};
use kira_source::Span;

/// One trait's existential: its synthesized enum, the variants it filled from
/// the conformance table, and the dispatchers its call sites reserved.
pub(crate) struct TraitExistential {
    /// The synthesized enum every reference resolves to.
    pub(crate) enum_id: EnumId,
    /// One variant per conforming type, in first-conformance order.
    pub(crate) variants: Vec<ConstructVariant>,
    /// Per-member shapes and their reserved dispatchers.
    pub(crate) methods: BTreeMap<String, ExistentialMethod>,
}

/// One member's dispatch-facing signature.
pub(crate) struct ExistentialMethod {
    /// The written parameters, receiver excluded.
    pub(crate) params: Vec<Type>,
    /// The written result, `Void` when the declaration wrote none.
    pub(crate) result: Type,
    /// Whether the receiver is written `borrow mut self`.
    ///
    /// The dispatcher writes the receiver back through every frame exactly
    /// when this holds, and its call sites record one for it on the same
    /// terms a mutating method's call sites do.
    pub(crate) mutates_self: bool,
    /// The synthesized tag dispatcher, reserved at the first call site.
    pub(crate) dispatcher: Option<FuncId>,
}

impl Analyzer<'_> {
    /// The existential for a trait name, reserving it on first use.
    ///
    /// A compiler-known trait never reaches here — those classify no values,
    /// and the type resolver refuses them before asking. An unsafe trait (one
    /// whose members are not all reachable through a value) is refused here
    /// with [`KSEM313`] naming the first member that would need static
    /// dispatch, and reserves nothing.
    pub(crate) fn reserve_trait_existential(&mut self, name: &str, span: Span) -> Option<EnumId> {
        if let Some(existing) = self.trait_existentials.get(name) {
            return Some(existing.enum_id);
        }
        // Object safety: every member must be reachable through a value, which
        // means a `self` receiver. A member without one would need static
        // knowledge of the concrete type, and an existential has none.
        let declared = self.traits.get(name)?;
        let offender = declared
            .members
            .iter()
            .find_map(|member| (member.function.receiver.is_none()).then(|| member.name.clone()));
        if let Some(member) = offender {
            self.emit(
                span,
                "KSEM313",
                format!(
                    "`{name}` cannot be used as a type: member `{member}` takes no `self`, \
                     so a call through a value could not reach it"
                ),
            );
            return None;
        }
        let owner = self.imports.package_of(declared.source);
        let Some(enum_id) = self.program.types.enums_mut().declare_owned(
            owner,
            kira_semantics_model::EnumDef {
                name: format!("some {name}"),
                variants: Vec::new(),
            },
        ) else {
            self.emit(
                span,
                "KSEM006",
                format!("existential type `some {name}` is already defined"),
            );
            return None;
        };
        self.enum_defaults.push(Vec::new());
        self.trait_existentials.insert(
            name.to_owned(),
            TraitExistential {
                enum_id,
                variants: Vec::new(),
                methods: BTreeMap::new(),
            },
        );
        self.existential_traits.insert(enum_id, name.to_owned());
        Some(enum_id)
    }

    /// Whether `id` is a synthesized trait-existential enum.
    pub(crate) fn is_trait_existential_type(&self, id: EnumId) -> bool {
        self.existential_traits.contains_key(&id)
    }

    /// Fills every reserved existential from the final conformance table.
    ///
    /// Runs once all conformances are recorded — colon lists, impl blocks, and
    /// family claims alike — because membership *is* that table: one variant
    /// per distinct conforming type, in first-recording order, so the tags are
    /// deterministic across runs. Member shapes resolve here too, with the
    /// trait's own file as the signature context.
    pub(crate) fn fill_trait_existentials(&mut self) {
        let names: Vec<String> = self.trait_existentials.keys().cloned().collect();
        for name in names {
            self.fill_single_trait_existential(&name);
        }
    }

    /// Fills one existential — variants from the conformance table, member
    /// shapes from the trait's declaration. Idempotent, so a reservation that
    /// lands after the batch pass (a signature first resolved inside a body)
    /// fills itself at its first call or coercion.
    pub(crate) fn fill_single_trait_existential(&mut self, name: &str) {
        let Some(enum_id) = self.trait_existentials.get(name).map(|e| e.enum_id) else {
            return;
        };
        if self
            .trait_existentials
            .get(name)
            .is_some_and(|existing| !existing.variants.is_empty())
        {
            return;
        }
        let mut seen: Vec<Type> = Vec::new();
        for entry in &self.conformances {
            let ty = entry.ty;
            if entry.contract.trait_name() != Some(name) || seen.contains(&ty) {
                continue;
            }
            seen.push(ty);
        }
        let variants: Vec<ConstructVariant> = seen
            .iter()
            .copied()
            .enumerate()
            .map(|(tag, ty)| ConstructVariant {
                ty,
                tag: tag as u32,
            })
            .collect();
        let variant_defs: Vec<kira_semantics_model::VariantDef> = variants
            .iter()
            .map(|variant| kira_semantics_model::VariantDef {
                name: self.program.types.type_name(variant.ty),
                payload: Some(variant.ty),
            })
            .collect();
        self.program
            .types
            .enums_mut()
            .set_variants(enum_id, variant_defs);

        let shapes = self.required_shapes(name);
        let Some(existential) = self.trait_existentials.get_mut(name) else {
            return;
        };
        existential.variants = variants;
        existential.methods = shapes
            .unwrap_or_default()
            .into_iter()
            .map(|shape| {
                (
                    shape.name,
                    ExistentialMethod {
                        params: shape.params,
                        result: shape.result,
                        mutates_self: shape.receiver_mutates,
                        dispatcher: None,
                    },
                )
            })
            .collect();
    }

    /// Wraps a concrete value whose position expects a trait existential.
    ///
    /// Membership is the conformance table: a struct with a recorded row for
    /// the trait becomes the matching variant; anything else falls through so
    /// the position's own diagnostics speak.
    pub(crate) fn coerce_trait_existential(
        &mut self,
        value: HirExprId,
        expected: Type,
    ) -> Option<HirExprId> {
        let Type::Enum(existential_id) = expected else {
            return None;
        };
        let trait_name = self.existential_traits.get(&existential_id)?.clone();
        if self
            .trait_existentials
            .get(&trait_name)
            .is_some_and(|existing| existing.variants.is_empty())
        {
            self.fill_single_trait_existential(&trait_name);
        }
        let value_ty = self.program.expr(value).type_of();
        if !self.conforms_to(value_ty, &trait_name) {
            return None;
        }
        let existential = self.trait_existentials.get(&trait_name)?;
        let tag = existential
            .variants
            .iter()
            .find(|variant| variant.ty == value_ty)?
            .tag;
        Some(self.program.exprs.alloc(HirExpr::EnumNew {
            enum_id: existential_id,
            tag,
            payload: Some(value),
        }))
    }

    /// Type-checks a member call on a trait-existential value.
    ///
    /// The call lowers to the trait's synthesized tag dispatcher — the same
    /// balanced-tree shape a construct family builds — with each arm calling
    /// the concrete implementation that type's conformance provided. A member
    /// whose receiver is `borrow mut self` dispatches mutating: the call site
    /// records a receiver writeback, and every dispatcher frame hands the
    /// updated existential back through it.
    pub(crate) fn analyze_trait_existential_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        existential_id: EnumId,
        method: &str,
        content: crate::constructs::ConstructCallContent<'_>,
        span: Span,
    ) -> HirExprId {
        let crate::constructs::ConstructCallContent {
            args,
            children,
            receiver_syntax,
        } = content;
        let trait_name = match self.existential_traits.get(&existential_id) {
            Some(name) => name.clone(),
            None => return self.program.exprs.alloc(HirExpr::Error),
        };
        if self
            .trait_existentials
            .get(&trait_name)
            .is_some_and(|existing| existing.methods.is_empty())
        {
            self.fill_single_trait_existential(&trait_name);
        }
        let Some(shape) = self
            .trait_existentials
            .get(&trait_name)
            .and_then(|existing| existing.methods.get(method))
            .map(|known| (known.params.clone(), known.result, known.mutates_self))
        else {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            for child in children {
                self.analyze_expr(ctx, *child);
            }
            self.emit(
                span,
                "KSEM314",
                format!(
                    "`{}` has no member `{method}`",
                    self.type_name(Type::Enum(existential_id))
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let (params, result, mutates_self) = shape;
        // Reserve the dispatcher before borrowing the table immutably: one
        // lookup decides, then a second reads what the first may have written.
        let already = self
            .trait_existentials
            .get(&trait_name)
            .and_then(|existing| existing.methods.get(method))
            .and_then(|known| known.dispatcher);
        let dispatcher = match already {
            Some(dispatcher) => dispatcher,
            None => {
                let reserved = self.reserve_synth();
                if let Some(known) = self
                    .trait_existentials
                    .get_mut(&trait_name)
                    .and_then(|existing| existing.methods.get_mut(method))
                {
                    known.dispatcher = Some(reserved);
                }
                reserved
            }
        };
        let callable = format!("{}.{method}", self.type_name(Type::Enum(existential_id)));
        let positional = Analyzer::argument_slots(args);
        let mut values = vec![receiver];
        for (index, slot_value) in positional.iter().enumerate() {
            let Some(arg) = slot_value else {
                // Trait requirements carry no parameter defaults: an omitted
                // argument is an arity error, reported once below.
                for rest in positional.iter().skip(index + 1).flatten() {
                    self.analyze_expr(ctx, *rest);
                }
                break;
            };
            match params.get(index) {
                Some(&expected) => {
                    let value = self.analyze_expr_expecting(ctx, *arg, Some(expected));
                    values.push(value);
                }
                None => values.push(self.analyze_expr(ctx, *arg)),
            }
        }
        // Traits take no trailing content: their members are ordinary
        // functions, so children here are a mistake the families' refusal
        // already names.
        for child in children {
            self.analyze_expr(ctx, *child);
        }
        if !children.is_empty() {
            self.emit(
                span,
                "KSEM278",
                format!("`{callable}` takes no trailing content"),
            );
        }
        if values.len() != params.len() + 1 {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{callable}` takes {} argument(s), found {}",
                    params.len(),
                    values.len().saturating_sub(1)
                ),
            );
        } else {
            for (index, &expected) in params.iter().enumerate() {
                let Some(value) = values.get(index + 1).copied() else {
                    break;
                };
                let actual = self.program.expr(value).type_of();
                if actual != Type::Error && !self.admits(actual, expected) {
                    self.emit(
                        span,
                        "KSEM063",
                        format!(
                            "argument {} of `{callable}` expects `{}`, found `{}`",
                            index + 1,
                            self.type_name(expected),
                            self.type_name(actual)
                        ),
                    );
                }
                values[index + 1] = self.coerce_into(value, expected);
            }
        }
        // A mutating member writes the existential back into the caller's
        // storage, exactly as a mutating method on a struct does: the receiver
        // must name a mutable place, and the call carries one writeback for
        // it. Resolving here rather than from the analyzed value is what lets
        // `b.bump(4)` land in `b` and refuse a temporary with the same words a
        // direct call uses.
        let writebacks = if mutates_self {
            let Some(receiver_syntax) = receiver_syntax else {
                return self.program.exprs.alloc(HirExpr::Error);
            };
            match self.resolve_place(ctx, receiver_syntax, PlacePurpose::MutCall) {
                Some((place, _)) => vec![HirWriteback { param: 0, place }],
                None => return self.program.exprs.alloc(HirExpr::Error),
            }
        } else {
            Vec::new()
        };
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: values,
            ty: result,
            writebacks,
        })
    }

    /// Builds the body of every dispatcher a trait-existential call reserved.
    ///
    /// Runs inside the same fixpoint loop as the family builders, because a
    /// body can reserve further synth functions of its own.
    pub(crate) fn build_trait_dispatchers(&mut self) {
        let mut rows: Vec<(FuncId, String, String, EnumId)> = self
            .trait_existentials
            .iter()
            .flat_map(|(trait_name, existing)| {
                existing.methods.iter().filter_map(move |(method, known)| {
                    known.dispatcher.map(|dispatcher| {
                        (
                            dispatcher,
                            trait_name.clone(),
                            method.clone(),
                            existing.enum_id,
                        )
                    })
                })
            })
            .collect();
        rows.sort_by_key(|(dispatcher, _, _, _)| dispatcher.0);
        rows.retain(|(dispatcher, _, _, _)| self.synth_needs_body(*dispatcher));
        for (dispatcher, trait_name, method, enum_id) in rows {
            let function = self.trait_dispatcher_body(&trait_name, enum_id, &method);
            self.fill_synth(dispatcher, function);
        }
    }

    /// Builds one trait dispatcher: a balanced tag tree whose arms extract the
    /// payload and call the concrete implementation directly.
    ///
    /// A member whose requirement writes `borrow mut self` dispatches
    /// mutating: the root, every tree node, and every arm carry the flag, so
    /// each frame's caller hands it the receiver by reference and the mutated
    /// existential reaches the original binding.
    fn trait_dispatcher_body(
        &mut self,
        trait_name: &str,
        existential_id: EnumId,
        method: &str,
    ) -> HirFunction {
        let Some((variants, params, result, mutates_self)) = self
            .trait_existentials
            .get(trait_name)
            .and_then(|existing| {
                let method = existing.methods.get(method)?;
                Some((
                    existing.variants.clone(),
                    method.params.clone(),
                    method.result,
                    method.mutates_self,
                ))
            })
        else {
            // The member row itself vanished mid-build, so its receiver mode
            // is unknowable; the placeholder is never reached by a call.
            return self.empty_dispatcher(trait_name, method, Type::Void, 0, false);
        };
        let dispatch = DispatchMethod {
            family: trait_name,
            method,
            family_id: existential_id,
            params: &params,
            result,
            mutates_self,
        };
        let mut arms = Vec::with_capacity(variants.len());
        for variant in variants.iter().copied() {
            let Some((target, target_result)) = self.variant_method_target(variant, method) else {
                continue;
            };
            let arm_id = self.reserve_synth();
            let arm_function =
                self.construct_dispatch_arm_function(dispatch, variant, target, target_result);
            self.fill_synth(arm_id, arm_function);
            arms.push((variant, arm_id));
        }
        if arms.is_empty() {
            let mut ctx = FnCtx::new(result);
            let ownership = if mutates_self {
                OwnershipMode::BorrowMut
            } else {
                OwnershipMode::BorrowRead
            };
            ctx.declare_hidden_as(Type::Enum(existential_id), mutates_self, ownership);
            for &ty in &params {
                ctx.declare_hidden_as(ty, false, OwnershipMode::BorrowRead);
            }
            let body = if result == Type::Void {
                vec![self.program.stmts.alloc(HirStmt::Return { value: None })]
            } else {
                let value = self.default_value(result);
                vec![
                    self.program
                        .stmts
                        .alloc(HirStmt::Return { value: Some(value) }),
                ]
            };
            return HirFunction {
                name: format!("some {trait_name}.{method}$dispatch"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: ctx.locals,
                body,
                is_main: false,
                is_async: false,
                execution: Execution::Inherited,
                mutates_self,
                name_span: Span::new(0, 0),
            };
        }
        let Some((_, fallback)) = arms.last().copied() else {
            return HirFunction {
                name: format!("some {trait_name}.{method}$dispatch"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: Vec::new(),
                body: Vec::new(),
                is_main: false,
                is_async: false,
                execution: Execution::Inherited,
                mutates_self,
                name_span: Span::new(0, 0),
            };
        };
        arms.sort_by_key(|(variant, _)| variant.tag);
        let mut tree_number = 0;
        self.construct_dispatch_tree_function(
            dispatch,
            &arms,
            fallback,
            format!("some {trait_name}.{method}$dispatch"),
            &mut tree_number,
        )
    }

    /// The concrete implementation one variant presents for a member.
    ///
    /// Conformance mints every presented member as `{Type}.{member}`, defaults
    /// included, so the ordinary function lookup reaches them.
    fn variant_method_target(
        &self,
        variant: ConstructVariant,
        method: &str,
    ) -> Option<(FuncId, Type)> {
        let owner = self.program.types.type_name(variant.ty);
        self.lookup_function(&format!("{owner}.{method}"))
            .map(|(id, _, result)| (id, result))
    }

    /// A placeholder dispatcher for a table row that vanished mid-build — the
    /// families' builder keeps the same escape hatch for the same reason.
    fn empty_dispatcher(
        &self,
        trait_name: &str,
        method: &str,
        result: Type,
        param_count: u32,
        mutates_self: bool,
    ) -> HirFunction {
        HirFunction {
            name: format!("some {trait_name}.{method}$dispatch"),
            param_count,
            return_type: result,
            locals: Vec::new(),
            body: Vec::new(),
            is_main: false,
            is_async: false,
            execution: Execution::Inherited,
            mutates_self,
            name_span: Span::new(0, 0),
        }
    }
}
