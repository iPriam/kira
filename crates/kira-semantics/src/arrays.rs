//! Array expression analysis: literals, index reads, `.count`, and `.append`.
//!
//! The array surface is exactly **two members** — `.append(v)` and `.count` —
//! and that is not a subset of something larger: everything else is `KSEM101`.
//! Refusing the rest is what keeps invented surface out of the language.
//!
//! # Why `.append` resolves a place and `.count` does not
//!
//! Reading an array yields an **independent value** — the runtime copies it,
//! exactly as it copies a struct. So `.count`, which only reads, can take any
//! expression: measuring a copy gives the same answer as measuring the
//! original.
//!
//! `.append` mutates. Appending to a *read* would push onto a copy that is
//! dropped a moment later, and the write would vanish with no diagnostic
//! anywhere. So its receiver is resolved to a [`HirPlace`] — a local plus a
//! walk — and the write goes through that walk into the object the local
//! actually holds. `rows[0].xs.append(42)` landing in `rows` is that
//! resolution, not an optimization on top of it.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

impl Analyzer<'_> {
    /// Type-checks an array literal (`[1, 2, 3]`, `[]`).
    ///
    /// The element type comes from `expected` when the position supplies one
    /// (`var xs: [Int] = []`), and otherwise from the first element that
    /// resolves. Checking each element against *one* element type is what
    /// gives `[1, "a"]` and `[1, 2.0]` a diagnostic apiece rather than a
    /// silently widened type: [`Type::assignable_to`] is exact, so there is no
    /// numeric widening to fall into.
    pub(crate) fn analyze_array_literal(
        &mut self,
        ctx: &mut FnCtx,
        elements: &[ExprId],
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        // An expectation only helps when it *is* an array: `let n: Int = [1]`
        // expects `Int`, which says nothing about the element type, so the
        // elements decide and the binding reports the mismatch.
        let expected_element = expected.and_then(|ty| self.program.types.element_of(ty));

        let values: Vec<HirExprId> = elements
            .iter()
            .map(|&element| self.analyze_expr_expecting(ctx, element, expected_element))
            .collect();

        let Some(element_ty) = self.element_type(&values, expected_element, span) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };

        // Every element is checked against the one element type, at its own
        // span, so a literal with two bad elements reports twice.
        for (&value, &written) in values.iter().zip(elements.iter()) {
            let value_ty = self.program.expr(value).type_of();
            if !value_ty.assignable_to(element_ty) {
                let element_span = self.tree.expr(written).span();
                self.emit(
                    element_span,
                    "KSEM105",
                    format!(
                        "an array of `{}` cannot hold an element of type `{}` \
                         (Kira does not widen numbers)",
                        self.type_name(element_ty),
                        self.type_name(value_ty)
                    ),
                );
            }
        }

        let ty = self.program.types.array_of(element_ty);
        self.program.exprs.alloc(HirExpr::ArrayNew {
            ty,
            elements: values,
        })
    }

    /// Decides an array literal's element type, or `None` when it cannot be
    /// decided and the literal is an error.
    fn element_type(
        &mut self,
        values: &[HirExprId],
        expected_element: Option<Type>,
        span: Span,
    ) -> Option<Type> {
        if let Some(element) = expected_element {
            return Some(element);
        }
        // The first element that analyzed cleanly names the type. An element
        // that already failed says nothing about what the array holds.
        let inferred = values
            .iter()
            .map(|&value| self.program.expr(value).type_of())
            .find(|ty| *ty != Type::Error);
        if let Some(ty) = inferred {
            return Some(ty);
        }
        if values.is_empty() {
            // `[]` with nothing to infer from. Guessing an element type here
            // would invent one; the fix is to say it.
            self.emit(
                span,
                "KSEM104",
                "cannot tell what `[]` holds here; annotate the binding \
                 (`var xs: [Int] = []`)",
            );
        }
        // Every element failed to analyze: they already spoke, so this does not.
        None
    }

    /// Type-checks an index read (`xs[i]`).
    pub(crate) fn analyze_index(
        &mut self,
        ctx: &mut FnCtx,
        base: ExprId,
        index: ExprId,
        span: Span,
    ) -> HirExprId {
        let base_hir = self.analyze_expr(ctx, base);
        let base_ty = self.program.expr(base_hir).type_of();
        let index_hir = self.analyze_index_expr(ctx, index);

        let Some(element) = self.program.types.element_of(base_ty) else {
            // An error base already spoke; do not pile on.
            if base_ty != Type::Error {
                self.emit(
                    span,
                    "KSEM100",
                    format!(
                        "cannot index a value of type `{}`; only an array can be indexed",
                        self.type_name(base_ty)
                    ),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::Index {
            base: base_hir,
            index: index_hir,
            ty: element,
        })
    }

    /// Analyzes an index expression, requiring `Int`.
    ///
    /// Whether the index is *in range* is deliberately not asked: it is a
    /// runtime trap, not a static check, because an index is generally not a
    /// constant. Pretending otherwise would reject working programs.
    pub(crate) fn analyze_index_expr(&mut self, ctx: &mut FnCtx, index: ExprId) -> HirExprId {
        let span = self.tree.expr(index).span();
        let hir = self.analyze_expr(ctx, index);
        let ty = self.program.expr(hir).type_of();
        if ty != Type::INT && ty != Type::Error {
            self.emit(
                span,
                "KSEM102",
                format!(
                    "an array index must be an `Int`, found `{}`",
                    self.type_name(ty)
                ),
            );
        }
        hir
    }

    /// Type-checks `xs.<name>` where `xs` is an array — the property side of
    /// the two-member surface.
    ///
    /// `.count` is the only one. It is a **property**: `xs.count()` is not how
    /// it is written, and saying so is more useful than "no such member".
    pub(crate) fn analyze_array_property(
        &mut self,
        array: HirExprId,
        name: &str,
        span: Span,
    ) -> HirExprId {
        if name == "count" {
            return self.program.exprs.alloc(HirExpr::ArrayLen { array });
        }
        if name == "append" {
            self.emit(
                span,
                "KSEM101",
                "`append` is a method: write `xs.append(value)`",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.emit(span, "KSEM101", unsupported_member(name));
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// Type-checks `xs.<name>(args)` where `xs` is an array — the method side.
    ///
    /// `.append` is the only one, and it is the reason the *receiver syntax*
    /// rather than the analyzed receiver is what arrives here: a place has to
    /// be resolved from what was written.
    pub(crate) fn analyze_array_method(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        name: &str,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        if name == "count" {
            self.emit(
                method_span,
                "KSEM101",
                "`count` is a property: write `xs.count`, without parentheses",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if name != "append" {
            self.emit(method_span, "KSEM101", unsupported_member(name));
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.analyze_array_append(ctx, receiver, method_span, args)
    }

    /// Type-checks `xs.append(v)`.
    fn analyze_array_append(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method_span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        // The receiver is resolved to a place *first*, so `append` on something
        // that is not a place is refused before its argument is analyzed
        // against an element type there is no array to supply.
        let Some((place, place_ty)) = self.resolve_place(ctx, receiver, PlacePurpose::Append)
        else {
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let element = self.program.types.element_of(place_ty);

        if args.len() != 1 {
            self.emit(
                method_span,
                "KSEM103",
                format!("`append` takes exactly one argument, found {}", args.len()),
            );
            for &arg in args {
                self.analyze_expr_expecting(ctx, arg, element);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }

        let value = self.analyze_expr_expecting(ctx, args[0], element);
        let Some(element) = element else {
            // The place resolved but is not an array. `resolve_place` reports
            // the shape problems; this reports the type one.
            if place_ty != Type::Error {
                self.emit(
                    method_span,
                    "KSEM101",
                    format!("type `{}` has no method `append`", self.type_name(place_ty)),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let value_ty = self.program.expr(value).type_of();
        if !value_ty.assignable_to(element) {
            let span = self.tree.expr(args[0]).span();
            self.emit(
                span,
                "KSEM105",
                format!(
                    "cannot append a `{}` to an array of `{}`",
                    self.type_name(value_ty),
                    self.type_name(element)
                ),
            );
        }
        self.program
            .exprs
            .alloc(HirExpr::ArrayAppend { place, value })
    }
}

/// The message for a member an array does not have.
///
/// Names the whole surface rather than only rejecting: the array surface is
/// two members, and a reader who guessed `push` or `length` is better served by
/// being told what does exist than by being told what does not.
fn unsupported_member(name: &str) -> String {
    format!("an array has no `{name}`; an array has `.append(value)` and `.count`")
}
