//! Where a value crosses into a wider declared type.
//!
//! Two crossings live here, and they are the language's only two: a value whose
//! type stops being known ([`Type::Any`]), and a generic instantiation whose
//! *type arguments* stop being known (`Result<Int, E>` where `Result<Any, E>` is
//! written). [`Analyzer::admits`] answers whether a position accepts a value and
//! [`Analyzer::coerce_into`] inserts the node that makes it true, and the two are
//! one pair on purpose: a position that admits a value without inserting the
//! node would hand a backend a value in the wrong machine form.
//!
//! Every position that admits a value of a *declared* type routes through the
//! pair — a `let` with an annotation, an assignment, a `return`, a call
//! argument, a struct field, an enum payload, an array element. A caller may
//! route *every* value through [`Analyzer::coerce_into`] rather than deciding
//! first, which is what keeps a position from being forgotten.
//!
//! # Why this is not left to the backends
//!
//! A backend could compare each expression's type against its destination's and
//! notice the same thing. Two backends doing that independently is two chances
//! to disagree, and the disagreement would be silent: the VM would keep working
//! (its values are tagged either way) while native code read a scalar as a box
//! pointer. Making the crossing a node in the tree means both engines are handed
//! the same answer instead of deriving it.
//!
//! # Why there is no way back
//!
//! The language has no `is`, no `as`, and no downcast form, so nothing can ask
//! an `Any` what it holds — see [`Type::Any`] — and nothing can ask a
//! `Result<Any, E>` to become the `Result<Int, E>` it was built from. Both
//! crossings are deliberately one-directional, and will stay so until the
//! language grows the surface that would justify the other half.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// Whether a value of `actual` may be used where `expected` is declared.
    ///
    /// The lattice's own rule ([`Type::assignable_to`]) plus the one rule that
    /// needs the program's tables to answer: an instantiation of a generic enum
    /// widening into another instantiation of the same template. Every position
    /// that pairs its check with [`Analyzer::coerce_into`] asks this rather than
    /// `assignable_to`, so the check and the conversion admit the same set.
    pub(crate) fn admits(&self, actual: Type, expected: Type) -> bool {
        self.program.types.admits(actual, expected)
    }

    /// [`Analyzer::admits`], plus the subclass a *call argument* may be.
    ///
    /// Only an argument, because an argument is the only position whose callee
    /// is specialized for the concrete class — see
    /// `Analyzer::specialize_callables`. Everywhere else a subclass would widen
    /// into a parent-typed binding and lose the type that picks the override:
    ///
    /// ```text
    /// let a: Animal = Dog {}   // `a.speak()` would run Animal's
    /// let pack: [Animal] = […] // every element would run Animal's
    /// ```
    ///
    /// Both are refused rather than silently answered wrong, which is what
    /// admitting them everywhere did. They become legal when a value can carry
    /// its concrete class across the widening and dispatch on it.
    pub(crate) fn admits_argument(&self, actual: Type, expected: Type) -> bool {
        self.admits(actual, expected) || self.is_subclass_of(actual, expected)
    }

    /// Whether `actual` is a class that inherits from the class `expected`.
    ///
    /// The one crossing the type lattice cannot answer on its own, because a
    /// class is a struct by the time it reaches the table and which struct
    /// inherits which is analysis's own record.
    ///
    /// Needs no conversion node, unlike the other two crossings: a class's
    /// fields are flattened with its parents' first, so a subclass already
    /// *has* the parent's layout as a prefix and a position expecting the
    /// parent reads exactly the slots it means to.
    pub(crate) fn is_subclass_of(&self, actual: Type, expected: Type) -> bool {
        let (Type::Struct(descendant), Type::Struct(ancestor)) = (actual, expected) else {
            return false;
        };
        descendant != ancestor
            && self
                .classes
                .get(&descendant)
                .is_some_and(|info| info.ancestors.contains(&ancestor))
    }

    /// Carries `expr` into `expected`, inserting the crossing when there is one.
    ///
    /// Returns `expr` unchanged for every destination it already has the machine
    /// form of, and for a value that already failed to analyze.
    ///
    /// This never reports: an unassignable value is its caller's diagnostic to
    /// raise, and wrapping one here would only hide it behind a conversion.
    pub(crate) fn coerce_into(&mut self, expr: HirExprId, expected: Type) -> HirExprId {
        let from = self.program.expr(expr).type_of();
        if expected == Type::Any {
            if !from.erases_into_any() {
                return expr;
            }
            return self
                .program
                .exprs
                .alloc(HirExpr::IntoAny { value: expr, from });
        }
        if !self.program.types.widens_to(from, expected) {
            return expr;
        }
        self.program.exprs.alloc(HirExpr::Widen {
            value: expr,
            from,
            to: expected,
        })
    }
}
