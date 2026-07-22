//! Construction-time analysis of a construct-backed declaration: filling its
//! stored fields from a construction call's arguments and its child slots from
//! the trailing content block, and reading its computed bridge member.
//!
//! The declaration side — collecting families, declaring each backed
//! declaration as a struct, and recording its child slots — lives in the parent
//! module. Here a *use* of one is type-checked and lowered: every path ends in
//! the same [`HirExpr::StructNew`](kira_semantics_model::hir::HirExpr::StructNew)
//! a struct literal produces, so every backend runs a constructed widget — its
//! children included — unchanged.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId};

use super::ContentSlot;
use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
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

        // The trailing children fill the declaration's child slots. Done before
        // the default sweep so a slot field arrives already set rather than
        // being reported as a missing input.
        self.fill_content_slots(ctx, id, children, &name, &mut initializers, span);

        // Every slot is filled: a param from its argument, and each remaining
        // slot — an unset param or an own field — from its declared default.
        let mut slot = 0u32;
        while (slot as usize) < field_count {
            let index = slot as usize;
            slot += 1;
            if initializers[index].is_some() {
                continue;
            }
            let filled = match self.resolve_field_default(id, index as u32) {
                Some(default) => default,
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

    /// Fills a construction's child slots from the trailing content block's
    /// children, checking each child against the slot's element type.
    ///
    /// A single slot (`some X`) takes exactly one child; a list slot
    /// (`[some X]`) takes an ordered array of them. Children on a declaration
    /// with no slot, or a bare block for more than one slot (which would need
    /// named fills), are refused — the latter is the still-deferred boundary.
    fn fill_content_slots(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        children: &[ExprId],
        name: &str,
        initializers: &mut [Option<HirExprId>],
        span: Span,
    ) {
        let slots = self
            .constructs
            .get(&id)
            .map(|info| info.slots.clone())
            .unwrap_or_default();
        if slots.is_empty() {
            if !children.is_empty() {
                for &child in children {
                    self.analyze_expr(ctx, child);
                }
                self.emit(
                    span,
                    "KSEM229",
                    format!(
                        "`{name}` declares no child slot, so it takes no trailing child content"
                    ),
                );
            }
            return;
        }
        if slots.len() > 1 {
            for &child in children {
                self.analyze_expr(ctx, child);
            }
            for slot in &slots {
                initializers[slot.field_index as usize] =
                    Some(self.program.exprs.alloc(HirExpr::Error));
            }
            self.emit(
                span,
                "KSEM230",
                format!(
                    "`{name}` has more than one child slot; a bare content block is ambiguous — \
                     named child fills are not executable yet"
                ),
            );
            return;
        }
        let slot = slots[0].clone();
        if slot.list {
            let mut elements = Vec::with_capacity(children.len());
            for &child in children {
                let value = self.analyze_expr_expecting(ctx, child, Some(slot.element_ty));
                self.check_child_type(child, value, &slot, name);
                elements.push(value);
            }
            let array = self.program.exprs.alloc(HirExpr::ArrayNew {
                ty: slot.field_ty,
                elements,
            });
            initializers[slot.field_index as usize] = Some(array);
            return;
        }
        // A single slot takes exactly one child.
        if children.len() == 1 {
            let value = self.analyze_expr_expecting(ctx, children[0], Some(slot.element_ty));
            self.check_child_type(children[0], value, &slot, name);
            initializers[slot.field_index as usize] = Some(value);
            return;
        }
        for &child in children {
            self.analyze_expr(ctx, child);
        }
        self.emit(
            span,
            "KSEM231",
            format!(
                "child slot `{}` of `{name}` holds exactly one child, found {}",
                slot.name,
                children.len()
            ),
        );
        initializers[slot.field_index as usize] = Some(self.program.exprs.alloc(HirExpr::Error));
    }

    /// Reports a child whose type does not satisfy the slot's element type.
    fn check_child_type(
        &mut self,
        child: ExprId,
        value: HirExprId,
        slot: &ContentSlot,
        name: &str,
    ) {
        let actual = self.program.expr(value).type_of();
        if !actual.assignable_to(slot.element_ty) {
            let child_span = self.tree.expr(child).span();
            self.emit(
                child_span,
                "KSEM232",
                format!(
                    "child slot `{}` of `{name}` holds `{}`, but this child is `{}`",
                    slot.name,
                    self.type_name(slot.element_ty),
                    self.type_name(actual)
                ),
            );
        }
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
