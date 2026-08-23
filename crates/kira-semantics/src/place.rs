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
    /// The receiver of a call to a method that mutates its receiver
    /// (`g.mutate()`), which must name mutable storage so the mutation can be
    /// written back.
    MutCall,
    /// An argument passed to a `borrow mut` parameter (`step(tree, 1)`), which
    /// must name mutable storage so the callee's writes reach the caller.
    BorrowMut,
}

impl PlacePurpose {
    /// The message for a target that does not name storage at all.
    fn not_a_place(self) -> String {
        match self {
            PlacePurpose::Assign => {
                "the left side of an assignment must be a variable, a field, or an element"
                    .to_owned()
            }
            // A temporary is the likely mistake here, and naming it is more
            // use than restating the grammar.
            PlacePurpose::Append => {
                "`append` needs a variable to append to; appending to a temporary value \
                 would write to something that is discarded immediately"
                    .to_owned()
            }
            PlacePurpose::MutCall => {
                "a mutating method needs a variable to call it on; calling it on a temporary \
                 value would mutate something discarded immediately"
                    .to_owned()
            }
            PlacePurpose::BorrowMut => {
                "a `borrow mut` parameter needs a variable to write back into; passing a \
                 temporary value would mutate something discarded immediately"
                    .to_owned()
            }
        }
    }

    /// The diagnostic code for a target that does not name storage.
    ///
    /// The mutating-call case is a fresh code: it is a call that failed, not the
    /// left side of an assignment the reader never wrote.
    fn not_a_place_code(self) -> &'static str {
        match self {
            PlacePurpose::Assign | PlacePurpose::Append => "KSEM025",
            PlacePurpose::MutCall => "KSEM211",
            PlacePurpose::BorrowMut => "KSEM248",
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
            PlacePurpose::MutCall => format!(
                "cannot call a mutating method through immutable binding `{name}` \
                 (declare it with `var`)"
            ),
            PlacePurpose::BorrowMut => {
                format!("cannot mutably borrow immutable binding `{name}` (declare it with `var`)")
            }
        }
    }
}

/// Whether two places can name the same storage.
///
/// Two writes collide when one place is the other, or contains it: `ws` and
/// `ws.doc` overlap, and so do `ws.doc` and `ws.doc.nodes`. Two *sibling*
/// fields do not — `ws.doc` and `ws.world` are separate storage, which is what
/// lets one call write through both.
///
/// An array index is compared conservatively: `xs[i]` and `xs[j]` are treated
/// as the same element, because the indices are expressions and nothing here
/// evaluates them. Refusing a pair that might alias is the safe direction; the
/// alternative is a write silently erasing another.
pub(crate) fn places_overlap(left: &HirPlace, right: &HirPlace) -> bool {
    if left.local != right.local {
        return false;
    }
    left.path
        .iter()
        .zip(right.path.iter())
        .all(|(left, right)| match (left, right) {
            (HirPlaceStep::Field(left), HirPlaceStep::Field(right)) => left == right,
            (HirPlaceStep::Index(_), HirPlaceStep::Index(_)) => true,
            // A value is a struct or an array, never both, so a well-typed
            // program never walks one step of each into the same place.
            _ => false,
        })
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
        self.resolve_place_step(ctx, target, purpose, false)
    }

    /// One step of the place walk. `through_path` says this expression is a
    /// *base* the write reaches through rather than the target itself, which is
    /// the whole of the difference between reinitializing a binding and writing
    /// into the value it still holds.
    fn resolve_place_step(
        &mut self,
        ctx: &mut FnCtx,
        target: ExprId,
        purpose: PlacePurpose,
        through_path: bool,
    ) -> Option<(HirPlace, Type)> {
        match self.tree.expr(target).clone() {
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                // A name written inside a closure body may belong to an
                // enclosing frame, and a write is as much a use of it as a read
                // — so the same capture path answers both. Without this, a
                // closure assigning to a captured binding reported it undefined
                // instead of capturing it.
                let local = match self.resolve_capturing(ctx, &name, span) {
                    crate::closures::Captured::Local(local) => local,
                    crate::closures::Captured::Refused => return None,
                    crate::closures::Captured::Absent => {
                        self.emit(
                            span,
                            "KSEM023",
                            format!("cannot assign to undefined name `{name}`"),
                        );
                        return None;
                    }
                };
                // A moved-out local names no storage: whatever it held is gone.
                // Assigning to the binding *itself* is the exception — `tree =
                // step(move tree)` gives it a value again, and the caller marks
                // it live once the new value has been analyzed. Writing
                // *through* it (`tree.node = …`) still needs the value that is
                // gone, and so does an `append` or a mutating call.
                let reinitializes = !through_path && purpose == PlacePurpose::Assign;
                if !reinitializes && !self.check_local_live(ctx, local, span) {
                    return None;
                }
                // Reinitializing is ordinarily free of consequence, because the
                // value it replaces is dropped with its binding. A native-state
                // handle is the one value that is not: it names storage the
                // runtime releases only when told, so overwriting it is where
                // the box becomes unreachable.
                if reinitializes {
                    self.check_native_state_overwrite(ctx, local, span);
                }
                if !ctx.is_mutable(local) {
                    self.emit(span, "KSEM021", purpose.immutable_root(&name));
                    return None;
                }
                if let Some(binding) = ctx.binding_span(local) {
                    let definition = kira_source::FileSpan::new(self.source, binding);
                    self.link(span, definition);
                }
                // A boxed `var` names the box, not the value: replacing the
                // binding writes into the box, and writing *through* it reads
                // the value out, writes the copy, and stores it back. See
                // [`crate::cells`] for why that order is the semantics rather
                // than an implementation choice.
                if let Some(place) = self.cell_place(ctx, local, through_path, reinitializes) {
                    return Some(place);
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
                let (mut place, base_ty) = self.resolve_place_step(ctx, base, purpose, true)?;
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
                let (mut place, base_ty) = self.resolve_place_step(ctx, base, purpose, true)?;
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
                self.emit(
                    other.span(),
                    purpose.not_a_place_code(),
                    purpose.not_a_place(),
                );
                None
            }
        }
    }
}
