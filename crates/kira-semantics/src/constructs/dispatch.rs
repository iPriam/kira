//! Expected-type upcasts and synthesized dispatch for construct-family values.

use kira_semantics_model::hir::{
    Callee, FuncId, HirExpr, HirExprId, HirFunction, HirPlace, HirStmt, HirWriteback,
};
use kira_semantics_model::{EnumId, OwnershipMode, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId, TypeRef};

use crate::analyze::{Analyzer, FnCtx, InitContent};
use crate::place::PlacePurpose;

/// Written argument and child lists of one construct-family call.
pub(crate) struct ConstructCallContent<'a> {
    pub(crate) args: &'a [CallArg],
    pub(crate) children: &'a [ExprId],
}

/// The dispatch-facing signature of one construct-family method.
struct FamilyMethodShape {
    params: Vec<Type>,
    ownership: Vec<OwnershipMode>,
    /// Resolved parameter defaults, aligned with `params`. A `None` slot is
    /// mandatory; a `Some` fills a call that omits it.
    defaults: Vec<Option<HirExprId>>,
    /// The result every implementation presents, or `None` when the family
    /// stated the obligation without one — see
    /// [`ConstructFamilyMethod::constrained_result`].
    ///
    /// [`ConstructFamilyMethod::constrained_result`]: super::ConstructFamilyMethod::constrained_result
    result: Option<Type>,
    /// Whether the method's receiver is written `borrow mut self`.
    receiver_mutates: bool,
}

/// The family method a dispatcher is being generated for.
///
/// Every generated function — the selector tree's nodes and each concrete arm —
/// carries the same family, method name, erased receiver type and signature.
/// Threading them as one value keeps each generator's own parameters to what
/// actually varies between its calls.
#[derive(Clone, Copy)]
pub(crate) struct DispatchMethod<'family> {
    /// The construct family's declared name, as the generated function names
    /// spell it.
    pub(crate) family: &'family str,
    /// The family method's name.
    pub(crate) method: &'family str,
    /// The synthesized enum a receiver is erased to.
    pub(crate) family_id: EnumId,
    /// The method's parameter types, excluding the receiver.
    pub(crate) params: &'family [Type],
    /// The result every generated function presents.
    pub(crate) result: Type,
    /// Whether the method's requirement writes `borrow mut self`.
    ///
    /// Every generated function — root, tree node, and arm — carries the flag,
    /// so each frame's caller hands it the receiver by reference and a call
    /// that mutates reaches the original binding. Read-only dispatch leaves
    /// every frame taking its receiver by value, exactly as before.
    pub(crate) mutates_self: bool,
}

impl Analyzer<'_> {
    /// The synthesized enum for a construct family name.
    pub(crate) fn construct_family_type(&self, name: &str) -> Option<EnumId> {
        self.construct_families.get(name).map(|info| info.enum_id)
    }

    /// Whether `id` is a synthesized construct-family enum.
    pub(crate) fn is_construct_family_type(&self, id: EnumId) -> bool {
        self.construct_family_names.contains_key(&id)
    }

    /// Wraps a concrete construct-backed value when its position expects that
    /// declaration's heterogeneous family type.
    pub(crate) fn coerce_construct_value(
        &mut self,
        value: HirExprId,
        expected: Option<Type>,
    ) -> HirExprId {
        let Some(Type::Enum(expected_family)) = expected else {
            return value;
        };
        // A trait existential wraps any conforming type, membership decided by
        // the conformance table; see `crate::traits::existential`.
        if let Some(wrapped) = self.coerce_trait_existential(value, Type::Enum(expected_family)) {
            return wrapped;
        }
        if !self.is_construct_family_type(expected_family) {
            return value;
        }
        let Type::Struct(struct_id) = self.program.expr(value).type_of() else {
            return value;
        };
        // A declaration backed by a family that extends others is a variant of
        // each, so the tag is chosen by which family the position asked for.
        let Some((family, tag)) = self
            .constructs
            .get(&struct_id)
            .and_then(|info| {
                info.families
                    .iter()
                    .find(|(enum_id, _)| *enum_id == expected_family)
            })
            .copied()
        else {
            return value;
        };
        self.program.exprs.alloc(HirExpr::EnumNew {
            enum_id: family,
            tag,
            payload: Some(value),
        })
    }

    /// Whether a family exposes `name` as a computed property.
    pub(crate) fn construct_family_computed_member(&self, id: EnumId, name: &str) -> bool {
        let Some(family) = self.construct_family_names.get(&id) else {
            return false;
        };
        self.construct_families
            .get(family)
            .and_then(|info| info.methods.get(name))
            .is_some_and(|method| method.computed)
    }

    /// Type-checks a method call on a family value.
    ///
    /// A method whose requirement writes `borrow mut self` dispatches
    /// mutating: the call site records a receiver writeback, so the value the
    /// chosen variant mutated reaches the caller's binding.
    pub(crate) fn analyze_construct_family_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        receiver_syntax: Option<ExprId>,
        family_id: EnumId,
        method: &str,
        content: ConstructCallContent<'_>,
        span: Span,
    ) -> HirExprId {
        let ConstructCallContent { args, children } = content;
        let Some(shape) = self.family_method_shape(family_id, method) else {
            self.emit(
                span,
                "KSEM097",
                format!(
                    "construct family `{}` has no method `{method}`",
                    self.type_name(Type::Enum(family_id))
                ),
            );
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let FamilyMethodShape {
            params,
            ownership,
            defaults,
            result,
            receiver_mutates,
        } = shape;
        // A requirement written without `-> T` says nothing about what an
        // implementation returns, so there is no one type this call could have.
        // Reaching the member through the concrete declaration still works; only
        // dispatch through the family value cannot be typed.
        let Some(result) = result else {
            self.emit(
                span,
                "KSEM241",
                format!(
                    "`{}` declares `{method}` without a result type, so a call through the \
                     family value has no type; give the requirement a result type, or call \
                     `{method}` on the concrete declaration",
                    self.type_name(Type::Enum(family_id))
                ),
            );
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        // A uniform `extend` modifier has one body called directly; a
        // per-variant method is reached through a synthesized tag dispatcher.
        let callee = if self.family_method_is_uniform(family_id, method) {
            self.family_uniform_body(family_id, method)
        } else {
            self.construct_dispatcher_for(family_id, method)
        };
        let Some(dispatcher) = callee else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let callable = format!("{}.{}", self.type_name(Type::Enum(family_id)), method);
        // A method call binds by position, label or no label.
        let positional = Analyzer::argument_slots(args);
        let mut values = vec![receiver];
        for (index, slot_value) in positional.into_iter().enumerate() {
            let Some(arg) = slot_value else {
                // An omitted argument takes its parameter's default; without one
                // the missing/arity diagnostic already spoke, so stand in with
                // an error value that keeps the shape honest.
                let filled = defaults
                    .get(index)
                    .copied()
                    .flatten()
                    .unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error));
                values.push(filled);
                continue;
            };
            match (params.get(index), ownership.get(index)) {
                (Some(&expected), Some(&mode)) => {
                    values.push(self.analyze_call_argument(ctx, arg, expected, mode, &callable))
                }
                _ => values.push(self.analyze_expr(ctx, arg)),
            }
        }
        // The trailing block fills the method's content parameter, which is its
        // last — the children are written after every parenthesized argument,
        // so they fill the slot that follows them. This is what lets a modifier
        // read the way SwiftUI's do: `.toolbar { … }` rather than
        // `.toolbar(SomeContainer() { … })`.
        if !children.is_empty() {
            match self.family_method_content(family_id, method) {
                Some(content) => {
                    let value = self.content_value(ctx, &content, children, span);
                    values.push(value);
                }
                None => {
                    for &child in children {
                        self.analyze_expr(ctx, child);
                    }
                    self.emit(
                        span,
                        "KSEM278",
                        format!(
                            "`{callable}` takes no trailing content; give it a last parameter of                              `some X` for one child or `[some X]` for a list of them"
                        ),
                    );
                }
            }
        }
        // A positional call that omitted trailing arguments fills them from
        // their defaults, left to right, stopping at the first with none.
        while values.len() - 1 < params.len() {
            match defaults.get(values.len() - 1).copied().flatten() {
                Some(default) => values.push(default),
                None => break,
            }
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
                if !self.admits(actual, expected) {
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
                // An `Any` parameter of a family method takes the erased form,
                // exactly as a direct call to the same member would.
                values[index + 1] = self.coerce_into(value, expected);
            }
        }
        let writebacks = if receiver_mutates {
            match receiver_syntax {
                Some(receiver_syntax) => {
                    match self.resolve_place(ctx, receiver_syntax, PlacePurpose::MutCall) {
                        Some((place, _)) => vec![HirWriteback { param: 0, place }],
                        None => return self.program.exprs.alloc(HirExpr::Error),
                    }
                }
                // A qualified call on a declaration can be the declaration's
                // own receiver (`Family.method()` inside the declaration). Its
                // analyzed receiver is the hidden `self` local, so it still
                // has a real writeback place even though there is no source
                // expression to resolve. An out-of-context qualified call
                // supplies its syntax above and is checked as a temporary.
                None => match self.program.expr(receiver) {
                    HirExpr::Local { local, .. } => vec![HirWriteback {
                        param: 0,
                        place: HirPlace {
                            local: *local,
                            path: Vec::new(),
                        },
                    }],
                    _ => return self.program.exprs.alloc(HirExpr::Error),
                },
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

    /// Reads a computed family method through property syntax.
    pub(crate) fn analyze_construct_family_property(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        receiver_syntax: ExprId,
        family_id: EnumId,
        method: &str,
        span: Span,
    ) -> HirExprId {
        self.analyze_construct_family_call(
            ctx,
            receiver,
            Some(receiver_syntax),
            family_id,
            method,
            ConstructCallContent {
                args: &[],
                children: &[],
            },
            span,
        )
    }

    /// The content parameter of a family method, when its last parameter is one.
    ///
    /// Derived from the declared syntax rather than stored, because `some X` and
    /// `Any X` resolve to the SAME type — the difference is only visible in the
    /// type reference the author wrote, so a resolved parameter list cannot
    /// answer this on its own.
    fn family_method_content(&mut self, family_id: EnumId, method: &str) -> Option<InitContent> {
        let family = self.construct_family_names.get(&family_id)?.clone();
        let entry = self.construct_families.get(&family)?.methods.get(method)?;
        let source = entry.source;
        let params = &entry.function.params;
        let slot = params.len().checked_sub(1)?;
        let param_ty = params[slot].ty;
        let (element_ref, list) = match self.tree.type_ref(param_ty) {
            TypeRef::SomeConstruct { .. } => (param_ty, false),
            TypeRef::Array { element, .. }
                if matches!(self.tree.type_ref(*element), TypeRef::SomeConstruct { .. }) =>
            {
                (*element, true)
            }
            _ => return None,
        };
        let previous = self.source;
        self.source = source;
        let element = self.resolve_type_ref(element_ref);
        self.source = previous;
        Some(InitContent {
            slot,
            list,
            element,
        })
    }

    fn family_method_shape(&self, family_id: EnumId, method: &str) -> Option<FamilyMethodShape> {
        let family = self.construct_family_names.get(&family_id)?;
        let method = self.construct_families.get(family)?.methods.get(method)?;
        Some(FamilyMethodShape {
            params: method.params.clone(),
            ownership: method.ownership.clone(),
            defaults: method.defaults.clone(),
            result: method.constrained_result(),
            receiver_mutates: method
                .function
                .receiver
                .is_some_and(|receiver| receiver.mutable),
        })
    }

    fn construct_dispatcher_for(&mut self, family_id: EnumId, method: &str) -> Option<FuncId> {
        let family = self.construct_family_names.get(&family_id)?.clone();
        if let Some(dispatcher) = self
            .construct_families
            .get(&family)
            .and_then(|info| info.methods.get(method))
            .and_then(|method| method.dispatcher)
        {
            return Some(dispatcher);
        }
        let dispatcher = self.reserve_synth();
        self.construct_families
            .get_mut(&family)?
            .methods
            .get_mut(method)?
            .dispatcher = Some(dispatcher);
        Some(dispatcher)
    }

    /// Validates that every concrete variant presents each family method with one
    /// dispatch-compatible signature.
    pub(crate) fn check_construct_method_signatures(&mut self) {
        let rows: Vec<_> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.methods
                    .iter()
                    // A uniform `extend` modifier has one shared body, so it is
                    // never conformance-checked against the concrete variants.
                    .filter(|(_, method)| !method.uniform)
                    .flat_map(move |(method_name, method)| {
                        info.variants.iter().map(move |variant| {
                            (
                                info.enum_id,
                                family.clone(),
                                method_name.clone(),
                                method.function.name_span,
                                method.source,
                                method.params.clone(),
                                // An obligation that wrote no result type
                                // constrains the parameters only.
                                method.constrained_result(),
                                *variant,
                            )
                        })
                    })
            })
            .collect();
        for (enum_id, family, method, span, source, expected_params, expected_result, variant) in
            rows
        {
            // A variant this family only reached through `extends` was checked
            // against the family it was written for, and `check_family_overrides`
            // is what guarantees that surface still satisfies this one. Checking
            // it again here would compare it against the parent's un-narrowed
            // signature and report a conformance failure that is not one.
            let Some(variant_struct_id) = variant.struct_id() else {
                continue;
            };
            let declared_here = self
                .constructs
                .get(&variant_struct_id)
                .and_then(|info| info.families.first())
                .is_some_and(|(declaring, _)| *declaring == enum_id);
            if !declared_here {
                continue;
            }
            self.source = source;
            let owner = self
                .program
                .types
                .type_name(Type::Struct(variant_struct_id));
            let qualified = format!("{owner}.{method}");
            let Some((id, actual_params, actual_result)) = self.lookup_function(&qualified) else {
                self.emit(
                    span,
                    "KSEM234",
                    format!(
                        "`{owner}` does not implement `{method}` required by construct family `{family}`"
                    ),
                );
                continue;
            };
            if actual_params.get(1..) != Some(expected_params.as_slice())
                || expected_result.is_some_and(|expected| actual_result != expected)
            {
                self.emit(
                    span,
                    "KSEM235",
                    format!("`{qualified}` does not match the signature of `{family}.{method}`"),
                );
            }
            let implements_mutates = self.mutates_self(id);
            let requires_mutates = self
                .construct_families
                .get(&family)
                .and_then(|info| info.methods.get(&method))
                .is_some_and(|method| {
                    method
                        .function
                        .receiver
                        .is_some_and(|receiver| receiver.mutable)
                });
            if implements_mutates != requires_mutates {
                self.emit(
                    self.sigs[id.0 as usize].name_span,
                    "KSEM235",
                    format!(
                        "`{qualified}` has a receiver mode different from `{family}.{method}`; \
                         both must use the same `borrow mut self` declaration"
                    ),
                );
            }
        }
    }

    /// Fills every dispatcher reserved by a family call site.
    pub(crate) fn build_construct_dispatchers(&mut self) {
        let mut rows: Vec<_> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.methods
                    .iter()
                    // A uniform modifier reuses the `dispatcher` slot for its own
                    // body, filled by `build_extend_methods`, not a tag dispatcher.
                    .filter(|(_, info)| !info.uniform)
                    .filter_map(move |(method, info)| {
                        info.dispatcher
                            .map(|dispatcher| (dispatcher, family.clone(), method.clone()))
                    })
            })
            .collect();
        rows.sort_by_key(|(dispatcher, _, _)| dispatcher.0);
        rows.retain(|(dispatcher, _, _)| self.synth_needs_body(*dispatcher));
        for (dispatcher, family, method) in rows {
            let function = self.construct_dispatcher_body(&family, &method);
            self.fill_synth(dispatcher, function);
        }
        // A `@Required let` read through the family value reserves its own
        // dispatcher; see `super::value_members`.
        self.build_family_field_dispatchers();
    }

    fn construct_dispatcher_body(&mut self, family: &str, method: &str) -> HirFunction {
        let Some((enum_id, variants, params, result, mutates_self)) =
            self.construct_families.get(family).and_then(|info| {
                let method = info.methods.get(method)?;
                Some((
                    info.enum_id,
                    info.variants.clone(),
                    method.params.clone(),
                    method.result,
                    method
                        .function
                        .receiver
                        .is_some_and(|receiver| receiver.mutable),
                ))
            })
        else {
            return self.empty_construct_dispatcher();
        };
        // Give each concrete implementation its own body first.  Keeping the
        // payload extraction and call out of the selector is what prevents one
        // large aggregate-returning function from collecting every arm's
        // spills into one native frame.
        let dispatch = DispatchMethod {
            family,
            method,
            family_id: enum_id,
            params: &params,
            result,
            mutates_self,
        };
        let mut arms = Vec::with_capacity(variants.len());
        for variant in variants.iter().copied() {
            let Some((target, target_result)) = self.construct_method_target(variant, method)
            else {
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
            // The parameters are declared even though nothing reads them, on the
            // same terms the tree below declares them: `param_count` counts the
            // receiver and every argument, and a function whose locals do not
            // hold them is one whose two halves disagree about its arity. The
            // hybrid loader checks exactly that and refuses the program —
            // "takes N parameters in the manifest and 0 in the bytecode half" —
            // for a family none of whose declarations implement the member.
            ctx.declare_hidden_as(
                Type::Enum(enum_id),
                mutates_self,
                if mutates_self {
                    OwnershipMode::BorrowMut
                } else {
                    OwnershipMode::BorrowRead
                },
            );
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
                name: format!("Any {family}.{method}$dispatch"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: ctx.locals,
                body,
                is_main: false,
                is_async: false,
                execution: kira_semantics_model::Execution::Inherited,
                mutates_self,
                name_span: Span::new(0, 0),
            };
        }

        // A linear selector avoids the giant aggregate-returning frame, but
        // its call depth is still proportional to the number of variants.  A
        // balanced tag tree keeps both the native frame and the runtime depth
        // bounded, while the original final arm remains the unknown-tag
        // fallback.
        //
        // The empty case returned above, so a last arm exists.
        let Some((_, fallback)) = arms.last().copied() else {
            return HirFunction {
                name: format!("Any {family}.{method}$dispatch"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: Vec::new(),
                body: Vec::new(),
                is_main: false,
                is_async: false,
                execution: kira_semantics_model::Execution::Inherited,
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
            format!("Any {family}.{method}$dispatch"),
            &mut tree_number,
        )
    }

    pub(super) fn empty_construct_dispatcher(&self) -> HirFunction {
        HirFunction {
            name: "<unreachable construct dispatcher>".to_owned(),
            param_count: 0,
            return_type: Type::Void,
            locals: Vec::new(),
            body: Vec::new(),
            is_main: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
        }
    }
}
