//! Expected-type upcasts and synthesized dispatch for `Any Family` values.

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
    param_names: Vec<Option<kira_core::Symbol>>,
    ownership: Vec<OwnershipMode>,
    result: Type,
}

/// The pieces one dispatcher branch forwards to a concrete method.
struct DispatchArm<'locals> {
    target: FuncId,
    receiver: LocalId,
    family: EnumId,
    variant: ConstructVariant,
    param_locals: &'locals [LocalId],
    params: &'locals [Type],
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
        let Some((family, tag)) = self.constructs.get(&struct_id).and_then(|info| info.family)
        else {
            return value;
        };
        if family != expected_family {
            return value;
        }
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

    /// Type-checks a method call on an `Any Family` value.
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
            param_names,
            ownership,
            result,
        } = shape;
        let Some(dispatcher) = self.construct_dispatcher_for(family_id, method) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let callable = format!("{}.{}", self.type_name(Type::Enum(family_id)), method);
        let positional = self.bind_call_arguments(args, 0, &param_names, &callable, span);
        let mut values = vec![receiver];
        for (index, slot_value) in positional.into_iter().enumerate() {
            let Some(arg) = slot_value else {
                values.push(self.program.exprs.alloc(HirExpr::Error));
                continue;
            };
            match (params.get(index), ownership.get(index)) {
                (Some(&expected), Some(&mode)) => {
                    values.push(self.analyze_call_argument(ctx, arg, expected, mode, &callable))
                }
                _ => values.push(self.analyze_expr(ctx, arg)),
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
            for (index, (&value, &expected)) in values.iter().skip(1).zip(params.iter()).enumerate()
            {
                let actual = self.program.expr(value).type_of();
                if !actual.assignable_to(expected) {
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
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: values,
            ty: result,
            writeback: None,
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
            param_names: method.param_names.clone(),
            ownership: method.ownership.clone(),
            result: method.result,
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
                info.methods.iter().flat_map(move |(method_name, method)| {
                    info.variants.iter().map(move |variant| {
                        (
                            family.clone(),
                            method_name.clone(),
                            method.function.name_span,
                            method.source,
                            method.params.clone(),
                            method.result,
                            *variant,
                        )
                    })
                })
            })
            .collect();
        for (family, method, span, source, expected_params, expected_result, variant) in rows {
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
                || actual_result != expected_result
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
                info.methods.iter().filter_map(move |(method, info)| {
                    info.dispatcher
                        .map(|dispatcher| (dispatcher, family.clone(), method.clone()))
                })
            })
            .collect();
        rows.sort_by_key(|(dispatcher, _, _)| dispatcher.0);
        for (dispatcher, family, method) in rows {
            let function = self.construct_dispatcher_body(&family, &method);
            self.fill_synth(dispatcher, function);
        }
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
        let mut ctx = FnCtx::new(result);
        let receiver = ctx.declare_hidden(Type::Enum(enum_id), false);
        let mut param_locals = Vec::with_capacity(params.len());
        for &ty in &params {
            param_locals.push(ctx.declare_hidden(ty, false));
        }

        let mut body = Vec::with_capacity(variants.len().max(1));
        for (index, variant) in variants.iter().enumerate() {
            let Some(target) = self.construct_method_target(*variant, method) else {
                continue;
            };
            let arm = self.construct_dispatch_arm(DispatchArm {
                target,
                receiver,
                family: enum_id,
                variant: *variant,
                param_locals: &param_locals,
                params: &params,
                result,
            });
            if index + 1 == variants.len() {
                body.extend(arm);
                break;
            }
            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(enum_id),
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
            body.push(self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: arm,
                else_body: Vec::new(),
            }));
        }
        if body.is_empty() {
            let tail = if result == Type::Void {
                HirStmt::Return { value: None }
            } else {
                let value = self.default_value(result);
                HirStmt::Return { value: Some(value) }
            };
            body.push(self.program.stmts.alloc(tail));
        }
        HirFunction {
            name: format!("Any {family}.{method}$dispatch"),
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
        }
    }

    fn construct_method_target(&self, variant: ConstructVariant, method: &str) -> Option<FuncId> {
        let owner = self
            .program
            .types
            .type_name(Type::Struct(variant.struct_id));
        self.lookup_function(&format!("{owner}.{method}"))
            .map(|(id, _, _)| id)
    }

    fn construct_dispatch_arm(&mut self, arm: DispatchArm<'_>) -> Vec<HirStmtId> {
        let DispatchArm {
            target,
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
            ty: result,
            writeback: None,
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

    fn empty_construct_dispatcher(&self) -> HirFunction {
        HirFunction {
            name: "<unreachable construct dispatcher>".to_owned(),
            param_count: 0,
            return_type: Type::Void,
            locals: Vec::new(),
            body: Vec::new(),
            is_main: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
        }
    }
}
