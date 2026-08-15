//! Filling a construction's child slots from its content.
//!
//! A declaration's slots (`some X` / `[some X]`, or the `@Content let` compat
//! spelling) are filled from two places that mean the same thing:
//!
//! ```text
//! NavigationSplitView { Sidebar() } detail: { Content() }
//! ```
//!
//! The bare trailing block fills the **first** declared slot; a named fill
//! (`detail: …`) fills the slot it names. A fill's value is either an anonymous
//! `{ … }` content block, whose items fill the slot exactly as a trailing
//! block's do, or an ordinary expression that becomes the slot's value
//! directly. A slot nobody filled takes its declared default, and a list slot
//! without one is empty.

use kira_semantics_model::hir::{HirExpr, HirExprId, HirPlace, HirStmt, HirStmtId, LocalId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, Expr, ExprId};

use super::ContentSlot;
use crate::analyze::{Analyzer, FnCtx};
use crate::stmt::fors::ForCursor;

/// Everything one construction wrote as child content.
pub(crate) struct ChildFills<'a> {
    /// The declaration's child slots, in declaration order.
    pub(crate) slots: &'a [ContentSlot],
    /// The bare children of the trailing content block.
    pub(crate) children: &'a [ExprId],
    /// The named fills, each already resolved to the slot index its label
    /// named, in written order.
    pub(crate) named: &'a [(usize, CallArg)],
}

impl Analyzer<'_> {
    /// The child slots of a construct-backed declaration, in declaration order.
    pub(crate) fn child_slots(&self, id: StructId) -> Vec<ContentSlot> {
        self.constructs
            .get(&id)
            .map(|info| info.slots.clone())
            .unwrap_or_default()
    }

    /// Fills every child slot of a construction from its trailing children and
    /// its named fills, leaving a slot nobody filled for the default sweep.
    pub(crate) fn fill_child_slots(
        &mut self,
        ctx: &mut FnCtx,
        fills: ChildFills<'_>,
        name: &str,
        initializers: &mut [Option<HirExprId>],
        span: Span,
    ) {
        let ChildFills {
            slots,
            children,
            named,
        } = fills;
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
        let mut filled = vec![false; slots.len()];
        for (index, fill) in named {
            let slot = &slots[*index];
            if filled[*index] {
                self.analyze_expr(ctx, fill.value);
                self.emit(
                    fill.label_span.unwrap_or(span),
                    "KSEM274",
                    format!(
                        "child slot `{}` of `{name}` is filled more than once",
                        slot.name
                    ),
                );
                continue;
            }
            filled[*index] = true;
            let slot = slot.clone();
            match self.tree.expr(fill.value).clone() {
                Expr::Content {
                    children: items,
                    span: block,
                } => self.fill_slot_from_children(ctx, &slot, &items, name, initializers, block),
                _ => self.fill_slot_from_value(ctx, &slot, fill.value, name, initializers),
            }
        }
        // A bare content block is the first slot's, which is what makes
        // `NavigationSplitView { … } detail: { … }` read the way it looks: the
        // block nobody named belongs to the slot declared first.
        if !children.is_empty() {
            let slot = slots[0].clone();
            if filled[0] {
                for &child in children {
                    self.analyze_expr(ctx, child);
                }
                self.emit(
                    span,
                    "KSEM274",
                    format!(
                        "child slot `{}` of `{name}` is filled more than once",
                        slot.name
                    ),
                );
            } else {
                filled[0] = true;
                self.fill_slot_from_children(ctx, &slot, children, name, initializers, span);
            }
        }
        // An unfilled list slot is empty rather than missing — a construction
        // that lists no children listed none. A single slot left unfilled falls
        // through to the default sweep, which applies its declared default or
        // reports it missing. An update arrives with every field already read
        // from the base, so a slot it did not restate keeps what it held.
        for (index, slot) in slots.iter().enumerate() {
            if filled[index]
                || !slot.list
                || slot.has_default
                || initializers[slot.field_index as usize].is_some()
            {
                continue;
            }
            initializers[slot.field_index as usize] =
                Some(self.program.exprs.alloc(HirExpr::ArrayNew {
                    ty: slot.field_ty,
                    elements: Vec::new(),
                }));
        }
    }

    /// Fills one slot from a run of content items — a trailing block's children
    /// or a named fill's anonymous `{ … }`.
    fn fill_slot_from_children(
        &mut self,
        ctx: &mut FnCtx,
        slot: &ContentSlot,
        children: &[ExprId],
        name: &str,
        initializers: &mut [Option<HirExprId>],
        span: Span,
    ) {
        if slot.list {
            self.fill_list_slot(ctx, slot, children, name, initializers);
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
        if children.len() == 1 {
            let value = self.analyze_expr_expecting(ctx, children[0], Some(slot.element_ty));
            self.check_child_type(children[0], value, slot, name);
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

    /// Fills a list slot (`[some X]`) from its content items.
    fn fill_list_slot(
        &mut self,
        ctx: &mut FnCtx,
        slot: &ContentSlot,
        children: &[ExprId],
        name: &str,
        initializers: &mut [Option<HirExprId>],
    ) {
        // A content block with no builder is a fixed array — the children in
        // order — so it stays a plain `ArrayNew` with nothing to run.
        if !children.iter().any(|&child| self.is_builder_item(child)) {
            let mut elements = Vec::with_capacity(children.len());
            for &child in children {
                let value = self.analyze_expr_expecting(ctx, child, Some(slot.element_ty));
                self.check_child_type(child, value, slot, name);
                elements.push(value);
            }
            let array = self.program.exprs.alloc(HirExpr::ArrayNew {
                ty: slot.field_ty,
                elements,
            });
            initializers[slot.field_index as usize] = Some(array);
            return;
        }
        // A builder builds the array at run time: a fresh mutable local starts
        // empty, each item appends to it, and the local becomes the slot's
        // value. The building statements are hoisted ahead of the statement
        // whose construction they fill, because a construction is an expression
        // and the HIR has no block-expression.
        let acc = ctx.declare_hidden(slot.field_ty, true);
        let empty = self.program.exprs.alloc(HirExpr::ArrayNew {
            ty: slot.field_ty,
            elements: Vec::new(),
        });
        let mut stmts = vec![self.program.stmts.alloc(HirStmt::Let {
            local: acc,
            init: empty,
        })];
        self.expand_content_items(ctx, children, acc, slot, name, &mut stmts);
        for stmt in stmts {
            ctx.hoist_stmt(stmt);
        }
        let read = self.program.exprs.alloc(HirExpr::Local {
            local: acc,
            ty: slot.field_ty,
        });
        initializers[slot.field_index as usize] = Some(read);
    }

    /// Fills one slot from an ordinary expression — `detail: DetailView { … }`
    /// or `detail: view`.
    ///
    /// The slot's stored type is the expectation, so a concrete declaration
    /// written here upcasts into the `some Family` the slot holds, and a list
    /// slot filled this way takes a whole array rather than one element.
    fn fill_slot_from_value(
        &mut self,
        ctx: &mut FnCtx,
        slot: &ContentSlot,
        value: ExprId,
        name: &str,
        initializers: &mut [Option<HirExprId>],
    ) {
        let analyzed = self.analyze_expr_expecting(ctx, value, Some(slot.field_ty));
        let actual = self.program.expr(analyzed).type_of();
        if actual != Type::Error && !actual.assignable_to(slot.field_ty) {
            self.emit(
                self.tree.expr(value).span(),
                "KSEM232",
                format!(
                    "child slot `{}` of `{name}` holds `{}`, but this fill is `{}`",
                    slot.name,
                    self.type_name(slot.field_ty),
                    self.type_name(actual)
                ),
            );
        }
        initializers[slot.field_index as usize] = Some(analyzed);
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
                    // The condition's own hoisted statements run before the
                    // branch, not inside whichever arm drains next.
                    out.extend(ctx.take_pending_stmts());
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
}
