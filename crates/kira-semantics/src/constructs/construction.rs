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

use kira_semantics_model::hir::{HirExpr, HirExprId, HirPlace, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, Expr, ExprId};

use super::ContentSlot;
use crate::analyze::{Analyzer, FnCtx};
use crate::stmt::fors::ForCursor;

/// One value a construction call may supply, positionally or by name.
///
/// Every stored field of a construct-backed declaration is one of these — the
/// parenthesized `Name(text: String)` params first, then the declared `let`
/// members — because Construct 2.0 says a construct expresses caller-provided
/// values as its fields. A child slot (`some X` / `[some X]`) is the one field
/// kind that is *not* here: a slot is filled only by bare children in the
/// trailing block, never by an argument.
struct ConstructInput {
    /// The input's index among the struct's fields.
    field_index: u32,
    /// The field's name, which is also the argument label that fills it.
    name: String,
    /// The type the supplied value must satisfy.
    ty: Type,
}

impl Analyzer<'_> {
    /// Every field of `id` a construction call may fill, in declaration order.
    ///
    /// Order is the positional signature, so it must stay declaration order:
    /// `PLeaf("leaf", 3)` fills the first two inputs. Child slots are skipped
    /// rather than filtered afterwards, which is what keeps a slot from ever
    /// being reachable positionally — a slot's field index still exists, so the
    /// index carried here is the field's, not this vector's.
    fn construct_input_slots(&self, id: StructId) -> Vec<ConstructInput> {
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

        let mut initializers: Vec<Option<HirExprId>> = vec![None; field_count];
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
            // A content block with no builder is a fixed array — the children in
            // order — so it stays a plain `ArrayNew` with nothing to run.
            if !children.iter().any(|&child| self.is_builder_item(child)) {
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
            // A builder builds the array at run time: a fresh mutable local
            // starts empty, each item appends to it, and the local becomes the
            // slot's value. The building statements are hoisted ahead of the
            // statement whose construction they fill, because a construction is
            // an expression and the HIR has no block-expression.
            let acc = ctx.declare_hidden(slot.field_ty, true);
            let empty = self.program.exprs.alloc(HirExpr::ArrayNew {
                ty: slot.field_ty,
                elements: Vec::new(),
            });
            let mut stmts = vec![self.program.stmts.alloc(HirStmt::Let {
                local: acc,
                init: empty,
            })];
            self.expand_content_items(ctx, children, acc, &slot, name, &mut stmts);
            for stmt in stmts {
                ctx.hoist_stmt(stmt);
            }
            let read = self.program.exprs.alloc(HirExpr::Local {
                local: acc,
                ty: slot.field_ty,
            });
            initializers[slot.field_index as usize] = Some(read);
            return;
        }
        // A single slot takes exactly one child, so a builder — which produces
        // any number — cannot fill it. Report it against the builder rather than
        // letting a count check speak vaguely about it.
        if let Some(&builder) = children.iter().find(|&&child| self.is_builder_item(child)) {
            self.analyze_expr(ctx, builder);
            self.emit(
                self.tree.expr(builder).span(),
                "KSEM242",
                format!(
                    "child slot `{}` of `{name}` holds exactly one child; a `For`/`if` builder \
                     fills only a list slot (`[some X]`)",
                    slot.name
                ),
            );
            initializers[slot.field_index as usize] =
                Some(self.program.exprs.alloc(HirExpr::Error));
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

    /// Whether `child` is a `For`/`if` builder item rather than a bare child.
    fn is_builder_item(&self, child: ExprId) -> bool {
        matches!(
            self.tree.expr(child),
            Expr::ContentFor { .. } | Expr::ContentIf { .. }
        )
    }

    /// Lowers a run of content items into statements that append each produced
    /// child to `acc`, the slot's array local.
    ///
    /// A bare child appends its value. A `For` reuses the `for`-each desugar,
    /// its loop body appending each iteration's children. An `if` becomes an
    /// [`HirStmt::If`] whose branches append theirs. The recursion is what lets
    /// a builder nest inside a builder, and every child is still checked against
    /// the slot's element type where it is written.
    fn expand_content_items(
        &mut self,
        ctx: &mut FnCtx,
        items: &[ExprId],
        acc: LocalId,
        slot: &ContentSlot,
        name: &str,
        out: &mut Vec<HirStmtId>,
    ) {
        for &item in items {
            match self.tree.expr(item).clone() {
                Expr::ContentFor {
                    binding,
                    binding_span,
                    iterable,
                    body,
                    span,
                } => {
                    let cursor = ForCursor {
                        name: binding,
                        span: binding_span,
                    };
                    self.analyze_for_each(
                        ctx,
                        cursor,
                        iterable,
                        span,
                        out,
                        |analyzer, ctx, out| {
                            analyzer.expand_content_items(ctx, &body, acc, slot, name, out);
                        },
                    );
                }
                Expr::ContentIf {
                    cond,
                    then_body,
                    else_body,
                    ..
                } => {
                    let cond_expr = self.analyze_condition(ctx, cond);
                    ctx.push_scope();
                    let mut then_out = Vec::new();
                    self.expand_content_items(ctx, &then_body, acc, slot, name, &mut then_out);
                    ctx.pop_scope();
                    ctx.push_scope();
                    let mut else_out = Vec::new();
                    self.expand_content_items(ctx, &else_body, acc, slot, name, &mut else_out);
                    ctx.pop_scope();
                    out.push(self.program.stmts.alloc(HirStmt::If {
                        cond: cond_expr,
                        then_body: then_out,
                        else_body: else_out,
                    }));
                }
                _ => {
                    let value = self.analyze_expr_expecting(ctx, item, Some(slot.element_ty));
                    // A child that is itself a construction with builder content
                    // hoisted its own building statements; they belong here — in
                    // this loop or branch, before the append that uses the value
                    // — not drained at the enclosing statement, which would lift
                    // them out of the control flow that guards them.
                    out.extend(ctx.take_pending_stmts());
                    self.check_child_type(item, value, slot, name);
                    let append = self.program.exprs.alloc(HirExpr::ArrayAppend {
                        place: HirPlace {
                            local: acc,
                            path: Vec::new(),
                        },
                        value,
                    });
                    out.push(self.program.stmts.alloc(HirStmt::Expr { expr: append }));
                }
            }
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
