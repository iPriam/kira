//! Copy/update construction of an existing concrete construct value.
//!
//! A braced name that resolves to a local construct value evaluates that local
//! once, copies its fields into a new `StructNew`, and replaces only the paths
//! written by `let` overrides. The result deliberately uses the same HIR nodes
//! as an ordinary construction, so VM, LLVM, hybrid, and WASM lowering need no
//! construct-specific update operation.

use kira_semantics_model::hir::{FieldOrder, HirExpr, HirExprId, HirStmt, LocalId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId};

use super::slots::ChildFills;
use crate::analyze::{Analyzer, FnCtx};

#[derive(Clone)]
struct UpdateOverride {
    path: Vec<String>,
    value: ExprId,
    span: Span,
    label_span: Span,
}

impl Analyzer<'_> {
    /// Whether `ty` is a concrete construct-backed struct, rather than the
    /// existential `Any Family` enum that intentionally has a separate rule.
    pub(crate) fn concrete_construct_id(&self, ty: Type) -> Option<StructId> {
        let Type::Struct(id) = ty else {
            return None;
        };
        self.constructs.contains_key(&id).then_some(id)
    }

    /// Type-checks `local { let field = value }` as a copy/update.
    ///
    /// The local is read into one hidden binding before any override or child
    /// expression is analyzed. Every unspecified field is then initialized by
    /// reading that hidden value, which makes the one-evaluation rule visible
    /// in HIR rather than relying on a backend to common-subexpression-eliminate
    /// repeated reads.
    pub(crate) fn analyze_construct_update(
        &mut self,
        ctx: &mut FnCtx,
        local: LocalId,
        args: &[CallArg],
        children: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let base_ty = self
            .cell_inner(ctx, local)
            .unwrap_or_else(|| ctx.local_type(local));
        let Some(id) = self.concrete_construct_id(base_ty) else {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            for &child in children {
                self.analyze_expr(ctx, child);
            }
            self.emit(
                span,
                "KSEM266",
                "a braced construct update needs a concrete construct-backed value",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };

        if !self.check_local_live(ctx, local, span) {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let base_read = self.read_local(ctx, local);
        let base_local = ctx.declare_hidden(base_ty, false);
        ctx.hoist_stmt(self.program.stmts.alloc(HirStmt::Let {
            local: base_local,
            init: base_read,
        }));
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: base_local,
            ty: base_ty,
        });

        let slots = self.child_slots(id);
        let mut named_fills: Vec<(usize, CallArg)> = Vec::new();
        let mut overrides = Vec::new();
        for arg in args {
            // A label naming a child slot restates that slot's content, so it
            // goes through the same filling an ordinary construction uses —
            // which is what lets an update write `detail: { … }` at all, since a
            // content block is not a value an override could carry.
            if let Some(label) = arg.label
                && let Some(slot) = slots
                    .iter()
                    .position(|slot| slot.name == self.interner.resolve(label))
            {
                named_fills.push((slot, arg.clone()));
                continue;
            }
            let Some(label) = arg.label else {
                self.analyze_expr(ctx, arg.value);
                self.emit(
                    arg.span,
                    "KSEM262",
                    "a construct update override must name a field",
                );
                continue;
            };
            let path: Vec<String> = self
                .interner
                .resolve(label)
                .split('.')
                .map(ToOwned::to_owned)
                .filter(|segment| !segment.is_empty())
                .collect();
            if path.is_empty() {
                self.analyze_expr(ctx, arg.value);
                self.emit(
                    arg.label_span.unwrap_or(arg.span),
                    "KSEM263",
                    "a construct update override needs a non-empty field path",
                );
                continue;
            }
            overrides.push(UpdateOverride {
                path,
                value: arg.value,
                span: arg.span,
                label_span: arg.label_span.unwrap_or(arg.span),
            });
        }

        let mut invalid = vec![false; overrides.len()];
        for left in 0..overrides.len() {
            for right in (left + 1)..overrides.len() {
                if paths_overlap(&overrides[left].path, &overrides[right].path) {
                    invalid[right] = true;
                    let left_path = overrides[left].path.join(".");
                    self.emit(
                        overrides[right].label_span,
                        "KSEM265",
                        format!(
                            "construct update paths `{left_path}` and `{}` overlap",
                            overrides[right].path.join(".")
                        ),
                    );
                }
            }
        }

        let mut fields = self.construct_field_reads(id, base);
        self.fill_child_slots(
            ctx,
            ChildFills {
                slots: &slots,
                children,
                named: &named_fills,
            },
            &self.program.types.type_name(base_ty),
            &mut fields,
            span,
        );
        let active: Vec<UpdateOverride> = overrides
            .iter()
            .enumerate()
            .filter(|(index, _)| !invalid[*index])
            .map(|(_, update)| update.clone())
            .collect();
        let result = self.update_struct_fields(ctx, base_ty, fields, &active);

        for (index, update) in overrides.iter().enumerate() {
            if invalid[index] {
                self.analyze_expr(ctx, update.value);
            }
        }
        result
    }

    /// Reads every field of `base` in declaration order.
    fn construct_field_reads(&mut self, id: StructId, base: HirExprId) -> Vec<Option<HirExprId>> {
        let Some(definition) = self.program.types.structs().get(id) else {
            return Vec::new();
        };
        let fields: Vec<(u32, Type)> = definition
            .fields
            .iter()
            .enumerate()
            .map(|(index, field)| (index as u32, field.ty))
            .collect();
        fields
            .into_iter()
            .map(|(index, ty)| Some(self.program.exprs.alloc(HirExpr::Field { base, index, ty })))
            .collect()
    }

    /// Applies all overrides for one struct level, recursively rebuilding
    /// nested struct fields when a path has more than one segment.
    fn update_struct_fields(
        &mut self,
        ctx: &mut FnCtx,
        struct_ty: Type,
        mut fields: Vec<Option<HirExprId>>,
        overrides: &[UpdateOverride],
    ) -> HirExprId {
        let Some(struct_id) = self.concrete_or_plain_struct(struct_ty) else {
            for update in overrides {
                self.analyze_expr(ctx, update.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };

        for (index, field) in self.struct_fields(struct_id).into_iter().enumerate() {
            let matching: Vec<UpdateOverride> = overrides
                .iter()
                .filter(|update| update.path.first().is_some_and(|name| name == &field.0))
                .map(|update| UpdateOverride {
                    path: update.path[1..].to_vec(),
                    value: update.value,
                    span: update.span,
                    label_span: update.label_span,
                })
                .collect();
            if matching.is_empty() {
                continue;
            }
            if matching.iter().any(|update| update.path.is_empty()) {
                let Some(update) = matching.iter().find(|update| update.path.is_empty()) else {
                    continue;
                };
                let value = self.analyze_expr_expecting(ctx, update.value, Some(field.1));
                let actual = self.program.expr(value).type_of();
                if !self.admits(actual, field.1) {
                    self.emit(
                        update.span,
                        "KSEM264",
                        format!(
                            "construct update field `{}` expects `{}`, found `{}`",
                            field.0,
                            self.type_name(field.1),
                            self.type_name(actual)
                        ),
                    );
                }
                fields[index] = Some(self.coerce_into(value, field.1));
                continue;
            }

            let Some(nested_id) = self.concrete_or_plain_struct(field.1) else {
                for update in matching {
                    self.analyze_expr(ctx, update.value);
                    self.emit(
                        update.label_span,
                        "KSEM268",
                        format!(
                            "construct update path `{}` cannot descend through `{}`",
                            update.path.join("."),
                            self.type_name(field.1)
                        ),
                    );
                }
                continue;
            };
            let Some(base_field) = fields.get(index).copied().flatten() else {
                continue;
            };
            let nested_fields = self.construct_field_reads(nested_id, base_field);
            fields[index] = Some(self.update_struct_fields(ctx, field.1, nested_fields, &matching));
        }

        for update in overrides {
            if self
                .struct_fields(struct_id)
                .iter()
                .all(|field| update.path.first() != Some(&field.0))
            {
                self.analyze_expr(ctx, update.value);
                self.emit(
                    update.label_span,
                    "KSEM267",
                    format!(
                        "construct update type `{}` has no field path `{}`",
                        self.type_name(struct_ty),
                        update.path.join(".")
                    ),
                );
            }
        }

        let values = fields
            .into_iter()
            .map(|value| value.unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error)))
            .collect();
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id,
            fields: values,
            order: FieldOrder::Declared,
        })
    }

    /// A struct type whose fields may be copied and updated. Nested fields may
    /// be ordinary data structs, not only construct-backed structs.
    fn concrete_or_plain_struct(&self, ty: Type) -> Option<StructId> {
        let Type::Struct(id) = ty else {
            return None;
        };
        let is_function = self.as_function_type(ty).is_some();
        (!is_function).then_some(id)
    }

    /// A struct's field names and types, copied out before semantic diagnostics
    /// borrow the analyzer mutably.
    fn struct_fields(&self, id: StructId) -> Vec<(String, Type)> {
        self.program
            .types
            .structs()
            .get(id)
            .map(|definition| {
                definition
                    .fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn paths_overlap(left: &[String], right: &[String]) -> bool {
    is_prefix(left, right) || is_prefix(right, left)
}

fn is_prefix(prefix: &[String], path: &[String]) -> bool {
    prefix.len() <= path.len() && prefix.iter().zip(path).all(|(left, right)| left == right)
}
