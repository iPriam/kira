//! Construction-time analysis of a construct-backed declaration: filling its
//! stored fields from a construction call's arguments and its child slots from
//! its content, and reading its computed bridge member.
//!
//! The declaration side — collecting families, declaring each backed
//! declaration as a struct, and recording its child slots — lives in the parent
//! module. Here a *use* of one is type-checked and lowered: every path ends in
//! the same [`HirExpr::StructNew`](kira_semantics_model::hir::HirExpr::StructNew)
//! a struct literal produces, so every backend runs a constructed widget — its
//! children included — unchanged.

use kira_semantics_model::hir::{FieldOrder, HirExpr, HirExprId, HirStmt};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId};

use super::slots::ChildFills;
use crate::analyze::{Analyzer, FnCtx};

/// One value a construction call may supply, positionally or by name.
///
/// Every stored field of a construct-backed declaration is one of these — the
/// parenthesized `Name(text: String)` params first, then the declared `let`
/// members — because Construct 2.0 says a construct expresses caller-provided
/// values as its fields. A child slot (`some X` / `[some X]`) is the one field
/// kind that is *not* here: a slot is filled by content — the trailing block's
/// children or a fill naming it — and never positionally.
pub(crate) struct ConstructInput {
    /// The input's index among the struct's fields.
    pub(crate) field_index: u32,
    /// The field's name, which is also the argument label that fills it.
    pub(crate) name: String,
    /// The type the supplied value must satisfy.
    pub(crate) ty: Type,
}

impl Analyzer<'_> {
    /// Every field of `id` a construction call may fill, in declaration order.
    ///
    /// Order is the positional signature, so it must stay declaration order:
    /// `PLeaf("leaf", 3)` fills the first two inputs. Child slots are skipped
    /// rather than filtered afterwards, which is what keeps a slot from ever
    /// being reachable positionally — a slot's field index still exists, so the
    /// index carried here is the field's, not this vector's.
    pub(crate) fn construct_input_slots(&self, id: StructId) -> Vec<ConstructInput> {
        let slots: Vec<u32> = self
            .constructs
            .get(&id)
            .map(|info| info.slots.iter().map(|slot| slot.field_index).collect())
            .unwrap_or_default();
        let Some(def) = self.program.types.structs().get(id) else {
            return Vec::new();
        };
        def.fields
            .iter()
            .enumerate()
            .filter(|(index, _)| !slots.contains(&(*index as u32)))
            .map(|(index, field)| ConstructInput {
                field_index: index as u32,
                name: field.name.clone(),
                ty: field.ty,
            })
            .collect()
    }

    /// Type-checks `Name(args) { children }`: a construct-backed declaration's
    /// construction.
    ///
    /// The params fill the leading fields — positionally or by parameter name —
    /// the trailing children fill the child slots, and any remaining field takes
    /// its declared default. The result is the same [`HirExpr::StructNew`] a
    /// struct literal or a class constructor produces, so downstream sees a
    /// fully initialized struct.
    pub(crate) fn analyze_construct_new(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        args: &[CallArg],
        children: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let name = self.program.types.type_name(Type::Struct(id));
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.len())
            .unwrap_or_default();
        let inputs = self.construct_input_slots(id);
        let input_count = inputs.len();
        let slots = self.child_slots(id);

        let mut initializers: Vec<Option<HirExprId>> = vec![None; field_count];
        let mut named_fills: Vec<(usize, CallArg)> = Vec::new();
        let mut next_positional = 0usize;
        for arg in args {
            // Which input this argument fills is decided *before* it is
            // analyzed, so the value is analyzed against the type that input
            // expects. That expectation is what upcasts a concrete `PLeaf` into
            // the `some PNode` an input declares, and what anchors a
            // leading-dot member — neither of which an unexpecting analysis of
            // the argument could produce.
            let input = match arg.label {
                Some(label) => {
                    let label = self.interner.resolve(label).to_owned();
                    // A label naming a child slot is a named child fill —
                    // `detail: { … }` or `detail: DetailView { … }` — not a
                    // construction input. It is held back and filled with the
                    // rest of the content, so one place decides what every slot
                    // holds.
                    if let Some(slot) = slots.iter().position(|slot| slot.name == label) {
                        named_fills.push((slot, arg.clone()));
                        continue;
                    }
                    match inputs.iter().position(|input| input.name == label) {
                        Some(input) => input,
                        None => {
                            self.analyze_expr(ctx, arg.value);
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
                    let input = next_positional;
                    next_positional += 1;
                    if input >= input_count {
                        self.analyze_expr(ctx, arg.value);
                        self.emit(
                            span,
                            "KSEM205",
                            format!(
                                "`{name}` takes {input_count} construction input(s), found more"
                            ),
                        );
                        continue;
                    }
                    input
                }
            };
            let value = self.analyze_expr_expecting(ctx, arg.value, Some(inputs[input].ty));
            let ConstructInput {
                field_index,
                name: field,
                ty: expected,
            } = &inputs[input];
            let index = *field_index as usize;
            if initializers[index].is_some() {
                self.emit(
                    span,
                    "KSEM206",
                    format!("construction input `{field}` of `{name}` is set more than once"),
                );
            }
            let expected = *expected;
            let actual = self.program.expr(value).type_of();
            if !self.admits(actual, expected) {
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
            initializers[index] = Some(self.coerce_into(value, expected));
        }

        // The trailing children and the named fills fill the declaration's child
        // slots. Done before the default sweep so a slot field arrives already
        // set rather than being reported as a missing input.
        self.fill_child_slots(
            ctx,
            ChildFills {
                slots: &slots,
                children,
                named: &named_fills,
            },
            &name,
            &mut initializers,
            span,
        );

        // Defaults are instance initializers, not declaration-global
        // expressions. An isolated scope prevents them from seeing caller
        // locals, while each completed field is bound before the next one is
        // analyzed. This is what makes `let second = first` work and makes a
        // forward reference fail at the declaration-owned expression.
        ctx.push_isolated_scope();
        let definitions: Vec<(String, Type)> = self
            .program
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
            .unwrap_or_default();
        for (index, initializer) in initializers.iter_mut().enumerate() {
            let Some((field, field_ty)) = definitions.get(index).cloned() else {
                continue;
            };
            let value = match initializer.take() {
                Some(value) => value,
                None => match self.field_default(id, index as u32) {
                    Some(default) => {
                        if field_ty == Type::Error {
                            self.program.exprs.alloc(HirExpr::Error)
                        } else if !self
                            .resolving_struct_defaults
                            .insert((id.index(), index as u32))
                        {
                            let previous_source = self.source;
                            self.source = default.source;
                            self.emit(
                                self.tree.expr(default.syntax).span(),
                                "KSEM213",
                                "construct member defaults recursively construct each other and have no finite value",
                            );
                            self.source = previous_source;
                            self.program.exprs.alloc(HirExpr::Error)
                        } else {
                            let previous_source = self.source;
                            self.source = default.source;
                            let value =
                                self.analyze_expr_expecting(ctx, default.syntax, Some(field_ty));
                            self.source = previous_source;
                            let actual = self.program.expr(value).type_of();
                            if actual != Type::Error && !self.admits(actual, field_ty) {
                                self.emit(
                                    self.tree.expr(default.syntax).span(),
                                    "KSEM207",
                                    format!(
                                        "construct member `{field}` of `{name}` expects `{}`, found `{}`",
                                        self.type_name(field_ty),
                                        self.type_name(actual)
                                    ),
                                );
                            }
                            self.resolving_struct_defaults
                                .remove(&(id.index(), index as u32));
                            self.coerce_into(value, field_ty)
                        }
                    }
                    None => {
                        self.emit(
                            span,
                            "KSEM208",
                            format!("construction of `{name}` is missing input `{field}`"),
                        );
                        self.program.exprs.alloc(HirExpr::Error)
                    }
                },
            };
            let local = ctx.declare(&field, field_ty, false);
            ctx.hoist_stmt(
                self.program
                    .stmts
                    .alloc(HirStmt::Let { local, init: value }),
            );
            *initializer = Some(self.program.exprs.alloc(HirExpr::Local {
                local,
                ty: field_ty,
            }));
        }
        ctx.pop_scope();
        let fields: Vec<HirExprId> = initializers
            .into_iter()
            .map(|value| value.unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error)))
            .collect();
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
            order: FieldOrder::Declared,
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
