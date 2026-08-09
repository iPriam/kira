//! Expected-type upcasts and synthesized dispatch for construct-family values.

use kira_semantics_model::hir::{
    Callee, FuncId, HirBinaryOp, HirExpr, HirExprId, HirFunction, HirStmt, HirStmtId, LocalId,
};
use kira_semantics_model::{EnumId, OwnershipMode, Type};
use kira_source::Span;
use kira_syntax_model::ast::CallArg;

use super::ConstructVariant;
use crate::analyze::{Analyzer, FnCtx};

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
}

/// The pieces one dispatcher branch forwards to a concrete method.
struct DispatchArm<'locals> {
    target: FuncId,
    /// What the concrete method returns, which a child family may have made
    /// more specific than the dispatcher's own result.
    target_result: Type,
    receiver: LocalId,
    family: EnumId,
    variant: ConstructVariant,
    param_locals: &'locals [LocalId],
    params: &'locals [Type],
    result: Type,
}

/// The family method a dispatcher is being generated for.
///
/// Every generated function — the selector tree's nodes and each concrete arm —
/// carries the same family, method name, erased receiver type and signature.
/// Threading them as one value keeps each generator's own parameters to what
/// actually varies between its calls.
#[derive(Clone, Copy)]
struct DispatchMethod<'family> {
    /// The construct family's declared name, as the generated function names
    /// spell it.
    family: &'family str,
    /// The family method's name.
    method: &'family str,
    /// The synthesized enum a receiver is erased to.
    family_id: EnumId,
    /// The method's parameter types, excluding the receiver.
    params: &'family [Type],
    /// The result every generated function presents.
    result: Type,
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
    pub(crate) fn analyze_construct_family_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        family_id: EnumId,
        method: &str,
        args: &[CallArg],
        span: Span,
    ) -> HirExprId {
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
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: values,
            ty: result,
            writebacks: Vec::new(),
        })
    }

    /// Reads a computed family method through property syntax.
    pub(crate) fn analyze_construct_family_property(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        family_id: EnumId,
        method: &str,
        span: Span,
    ) -> HirExprId {
        self.analyze_construct_family_call(ctx, receiver, family_id, method, &[], span)
    }

    fn family_method_shape(&self, family_id: EnumId, method: &str) -> Option<FamilyMethodShape> {
        let family = self.construct_family_names.get(&family_id)?;
        let method = self.construct_families.get(family)?.methods.get(method)?;
        Some(FamilyMethodShape {
            params: method.params.clone(),
            ownership: method.ownership.clone(),
            defaults: method.defaults.clone(),
            result: method.constrained_result(),
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
            let declared_here = self
                .constructs
                .get(&variant.struct_id)
                .and_then(|info| info.families.first())
                .is_some_and(|(declaring, _)| *declaring == enum_id);
            if !declared_here {
                continue;
            }
            self.source = source;
            let owner = self
                .program
                .types
                .type_name(Type::Struct(variant.struct_id));
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
            if self.mutates_self(id) {
                self.emit(
                    span,
                    "KSEM236",
                    format!(
                        "`{qualified}` mutates its receiver and cannot dispatch through `Any {family}` yet"
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
        let Some((enum_id, variants, params, result)) =
            self.construct_families.get(family).and_then(|info| {
                let method = info.methods.get(method)?;
                Some((
                    info.enum_id,
                    info.variants.clone(),
                    method.params.clone(),
                    method.result,
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
            let ctx = FnCtx::new(result);
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
                mutates_self: false,
                name_span: Span::new(0, 0),
            };
        }

        // A linear selector avoids the giant aggregate-returning frame, but
        // its call depth is still proportional to the number of variants.  A
        // balanced tag tree keeps both the native frame and the runtime depth
        // bounded, while the original final arm remains the unknown-tag
        // fallback.
        let Some((_, fallback)) = arms.last().copied() else {
            unreachable!("an empty family dispatcher was handled above");
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

    /// Builds one node in a balanced tag selector tree.
    fn construct_dispatch_tree_function(
        &mut self,
        dispatch: DispatchMethod<'_>,
        arms: &[(ConstructVariant, FuncId)],
        fallback: FuncId,
        name: String,
        tree_number: &mut u32,
    ) -> HirFunction {
        let DispatchMethod {
            family,
            method,
            family_id,
            params,
            result,
        } = dispatch;
        let mut ctx = FnCtx::new(result);
        // Dispatch only inspects the erased value and forwards the call.  Keep
        // the caller's value borrowed so every node can lend the same storage;
        // making this an owned parameter would clone/drop the full enum at
        // every branch in a native executable.
        let receiver =
            ctx.declare_hidden_as(Type::Enum(family_id), false, OwnershipMode::BorrowRead);
        let param_locals: Vec<_> = params
            .iter()
            .map(|&ty| ctx.declare_hidden_as(ty, false, OwnershipMode::BorrowRead))
            .collect();

        let body = if arms.len() == 1 {
            let (variant, arm) = arms[0];
            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(family_id),
            });
            let tag = self.program.exprs.alloc(HirExpr::EnumTag { value });
            let wanted = self
                .program
                .exprs
                .alloc(HirExpr::Int(i64::from(variant.tag)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::EqInt,
                lhs: tag,
                rhs: wanted,
                ty: Type::Bool,
            });
            let then_body = self.construct_dispatch_function_call(
                arm,
                receiver,
                family_id,
                &param_locals,
                params,
                result,
            );
            let else_body = if arm == fallback {
                then_body.clone()
            } else {
                self.construct_dispatch_function_call(
                    fallback,
                    receiver,
                    family_id,
                    &param_locals,
                    params,
                    result,
                )
            };
            vec![self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body,
                else_body,
            })]
        } else {
            let middle = arms.len() / 2;
            let pivot = arms[middle].0.tag;
            let left_id = self.reserve_synth();
            let left_number = *tree_number;
            *tree_number += 1;
            let left = self.construct_dispatch_tree_function(
                dispatch,
                &arms[..middle],
                fallback,
                format!("Any {family}.{method}$dispatch_tree{left_number}"),
                tree_number,
            );
            self.fill_synth(left_id, left);
            let right_id = self.reserve_synth();
            let right_number = *tree_number;
            *tree_number += 1;
            let right = self.construct_dispatch_tree_function(
                dispatch,
                &arms[middle..],
                fallback,
                format!("Any {family}.{method}$dispatch_tree{right_number}"),
                tree_number,
            );
            self.fill_synth(right_id, right);

            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(family_id),
            });
            let tag = self.program.exprs.alloc(HirExpr::EnumTag { value });
            let pivot = self.program.exprs.alloc(HirExpr::Int(i64::from(pivot)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::LtInt,
                lhs: tag,
                rhs: pivot,
                ty: Type::Bool,
            });
            let then_body = self.construct_dispatch_function_call(
                left_id,
                receiver,
                family_id,
                &param_locals,
                params,
                result,
            );
            let else_body = self.construct_dispatch_function_call(
                right_id,
                receiver,
                family_id,
                &param_locals,
                params,
                result,
            );
            vec![self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body,
                else_body,
            })]
        };
        HirFunction {
            name,
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
        }
    }

    /// Builds one concrete implementation arm in its own native function.
    ///
    /// The family dispatcher stays as a small tag test, while this helper owns
    /// the payload extraction and the call into the concrete implementation.
    /// The VM sees the same HIR operation sequence and therefore keeps the
    /// existing semantics; native code simply gets a real call boundary for
    /// stack-frame sizing.
    fn construct_dispatch_arm_function(
        &mut self,
        dispatch: DispatchMethod<'_>,
        variant: ConstructVariant,
        target: FuncId,
        target_result: Type,
    ) -> HirFunction {
        let DispatchMethod {
            family,
            method,
            family_id,
            params,
            result,
        } = dispatch;
        let mut ctx = FnCtx::new(result);
        let receiver =
            ctx.declare_hidden_as(Type::Enum(family_id), false, OwnershipMode::BorrowRead);
        let param_locals: Vec<_> = params
            .iter()
            .map(|&ty| ctx.declare_hidden_as(ty, false, OwnershipMode::BorrowRead))
            .collect();
        let body = self.construct_dispatch_arm(DispatchArm {
            target,
            target_result,
            receiver,
            family: family_id,
            variant,
            param_locals: &param_locals,
            params,
            result,
        });
        HirFunction {
            name: format!("Any {family}.{method}$dispatch_arm{}", variant.tag),
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
        }
    }

    /// Emits the small call/return body for one dispatcher arm.
    fn construct_dispatch_function_call(
        &mut self,
        callee: FuncId,
        receiver: LocalId,
        family: EnumId,
        param_locals: &[LocalId],
        params: &[Type],
        result: Type,
    ) -> Vec<HirStmtId> {
        let mut args = vec![self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Enum(family),
        })];
        for (&local, &ty) in param_locals.iter().zip(params.iter()) {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(callee),
            args,
            ty: result,
            writebacks: Vec::new(),
        });
        if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        }
    }

    fn construct_method_target(
        &self,
        variant: ConstructVariant,
        method: &str,
    ) -> Option<(FuncId, Type)> {
        let owner = self
            .program
            .types
            .type_name(Type::Struct(variant.struct_id));
        self.lookup_function(&format!("{owner}.{method}"))
            .map(|(id, _, result)| (id, result))
    }

    fn construct_dispatch_arm(&mut self, arm: DispatchArm<'_>) -> Vec<HirStmtId> {
        let DispatchArm {
            target,
            target_result,
            receiver,
            family,
            variant,
            param_locals,
            params,
            result,
        } = arm;
        let family_value = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Enum(family),
        });
        let concrete_ty = Type::Struct(variant.struct_id);
        let concrete = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: family_value,
            ty: concrete_ty,
        });
        let mut args = vec![concrete];
        for (&local, &ty) in param_locals.iter().zip(params.iter()) {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(target),
            args,
            ty: target_result,
            writebacks: Vec::new(),
        });
        // A child family may return something more specific than the family
        // this dispatcher belongs to promised, so the arm carries its answer up
        // to the declared result rather than relabelling it.
        let call = self.coerce_into(call, result);
        if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        }
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
