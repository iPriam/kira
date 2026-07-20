//! Resolving a written expression to a [`HirPlace`]: a local plus a walk.
//!
//! A place is what a write goes *through*. Reading a value copies it, so a
//! write has to name storage rather than a value — that is what a place is, and
//! it is why an assignment target and an `append` receiver both come here.
//!
//! # Every step must be writable, not just the last
//!
//! `b.size.x = 1` rewrites the `size` field's contents in place, so a `let`
//! anywhere along the path makes the whole write illegal. The walk checks each
//! step as it goes rather than checking only where it lands.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirPlace, HirPlaceStep};
use kira_syntax_model::ast::{Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};

/// What a place is being resolved *for*.
///
/// Only the diagnostics differ. They differ enough to be worth carrying: a
/// reader who wrote `makeRows().append(1)` is not helped by being told about
/// the left side of an assignment they did not write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PlacePurpose {
    /// The target of an assignment (`p.x = 1`).
    Assign,
    /// The receiver of `append` (`xs.append(1)`).
    Append,
}

impl PlacePurpose {
    /// The message for a target that does not name storage at all.
    fn not_a_place(self) -> &'static str {
        match self {
            PlacePurpose::Assign => {
                "the left side of an assignment must be a variable, a field, or an element"
            }
            // A temporary is the likely mistake here, and naming it is more
            // use than restating the grammar.
            PlacePurpose::Append => {
                "`append` needs a variable to append to; appending to a temporary value \
                 would write to something that is discarded immediately"
            }
        }
    }

    /// The message for a root binding that is immutable.
    fn immutable_root(self, name: &str) -> String {
        match self {
            PlacePurpose::Assign => {
                format!("cannot assign to immutable binding `{name}` (declare it with `var`)")
            }
            PlacePurpose::Append => {
                format!("cannot append to immutable binding `{name}` (declare it with `var`)")
            }
        }
    }
}

impl Analyzer<'_> {
    /// Resolves a written expression into a [`HirPlace`] plus the type stored
    /// there, or `None` when it does not name a writable place.
    pub(crate) fn resolve_place(
        &mut self,
        ctx: &mut FnCtx,
        target: ExprId,
        purpose: PlacePurpose,
    ) -> Option<(HirPlace, Type)> {
        match self.tree.expr(target).clone() {
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                let Some(local) = ctx.resolve(&name) else {
                    self.emit(
                        span,
                        "KSEM023",
                        format!("cannot assign to undefined name `{name}`"),
                    );
                    return None;
                };
                // A moved-out local names no storage: whatever it held is gone.
                if !self.check_local_live(ctx, local, span) {
                    return None;
                }
                if !ctx.is_mutable(local) {
                    self.emit(span, "KSEM021", purpose.immutable_root(&name));
                    return None;
                }
                if let Some(binding) = ctx.binding_span(local) {
                    let definition = kira_source::FileSpan::new(self.source, binding);
                    self.link(span, definition);
                }
                Some((
                    HirPlace {
                        local,
                        path: Vec::new(),
                    },
                    ctx.local_type(local),
                ))
            }
            Expr::Field {
                base,
                field,
                field_span,
                ..
            } => {
                let (mut place, base_ty) = self.resolve_place(ctx, base, purpose)?;
                let field_name = self.interner.resolve(field).to_owned();
                let (index, field_ty) = self.resolve_field(base_ty, &field_name, field_span)?;
                let mutable = match base_ty {
                    Type::Struct(id) => self
                        .program
                        .types
                        .structs()
                        .get(id)
                        .and_then(|def| def.field(index))
                        .is_some_and(|def| def.mutable),
                    _ => false,
                };
                if !mutable {
                    self.emit(
                        field_span,
                        "KSEM024",
                        format!(
                            "cannot assign to immutable field `{field_name}` of `{}` \
                             (declare it with `var`)",
                            self.type_name(base_ty)
                        ),
                    );
                    return None;
                }
                place.path.push(HirPlaceStep::Field(index));
                Some((place, field_ty))
            }
            Expr::Index { base, index, span } => {
                let (mut place, base_ty) = self.resolve_place(ctx, base, purpose)?;
                let Some(element) = self.program.types.element_of(base_ty) else {
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
                    return None;
                };
                // An array's elements are as writable as the array is: there is
                // no per-element `let`, so reaching the array through a mutable
                // path is the whole permission. Whether the index is *in range*
                // is a runtime trap — see `analyze_index_expr`.
                let index_hir = self.analyze_index_expr(ctx, index);
                place.path.push(HirPlaceStep::Index(index_hir));
                Some((place, element))
            }
            other => {
                self.emit(other.span(), "KSEM025", purpose.not_a_place());
                None
            }
        }
    }
}
