//! Where a value crosses into a wider declared type.
//!
//! One crossing lives here, and it is the language's only one: a value whose
//! type stops being known ([`Type::Any`]). [`Analyzer::admits`] answers whether
//! a position accepts a value and [`Analyzer::coerce_into`] inserts the node
//! that makes it true, and the two are one pair on purpose: a position that
//! admits a value without inserting the node would hand a backend a value in
//! the wrong machine form.
//!
//! # Why one generic specialization does not reach another
//!
//! `Result<Int, E>` is not admitted where `Result<Any, E>` is written, and
//! `Box<Child>` is not admitted where `Box<Parent>` is. A specialization's
//! identity is part of its nominal identity, and that identity reaches
//! dispatch, reflection, serialization, and the ABI, so a value that silently
//! became another specialization would answer `.type` with a type it was never
//! built as. Kira aggregates are mutable values as well, so a widening would
//! have to be a rebuild whose writes land in a copy. A program that wants the
//! other specialization builds it, one payload at a time.
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
//! # The way back
//!
//! `value is T` and `value as T` ask an erased value what it holds, so the
//! crossing into `Any` is reversible by a checked downcast rather than by an
//! implicit rule. Nothing turns a `Result<Any, E>` back into a
//! `Result<Int, E>`: the payload is reachable through the downcast, and the
//! specialization is rebuilt from it.

use kira_semantics_model::hir::{ConvertKind, HirExpr, HirExprId};
use kira_semantics_model::{IntSpelling, Type};
use kira_source::Span;

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// Whether a value of `actual` may be used where `expected` is declared.
    ///
    /// The lattice's own rule, and nothing else. Every position that pairs its
    /// check with [`Analyzer::coerce_into`] asks this rather than
    /// `assignable_to` directly, so the check and the conversion stay one pair
    /// as the lattice grows.
    pub(crate) fn admits(&self, actual: Type, expected: Type) -> bool {
        actual.assignable_to(expected)
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
        if let (Type::Int(from_spelling), Type::Int(to_spelling)) = (from, expected)
            && from_spelling != to_spelling
        {
            return self.narrow_int(expr, from_spelling, to_spelling);
        }
        expr
    }

    /// An integer reaching storage of another spelling.
    ///
    /// A bare literal that fits needs nothing; one that does not was already
    /// reported where it was typed ([`Analyzer::int_literal`]), and any that
    /// arrives here untyped converts at run time like every other value: an
    /// identity when every value of the source fits the destination, a
    /// checked narrowing otherwise.
    fn narrow_int(&mut self, expr: HirExprId, from: IntSpelling, to: IntSpelling) -> HirExprId {
        if let HirExpr::Int(value) = *self.program.expr(expr) {
            // A hexadecimal literal is a bit pattern, and the one way a
            // literal can be negative: as a `U64` it names the unsigned
            // value of those bits.
            let denoted = if to == IntSpelling::U64 && value < 0 {
                i128::from(value as u64)
            } else {
                i128::from(value)
            };
            if to.holds(denoted) {
                return expr;
            }
        }
        if from.widens_into(to) {
            return expr;
        }
        self.program.exprs.alloc(HirExpr::Convert {
            operand: expr,
            kind: ConvertKind::IntToInt,
            ty: Type::Int(to),
        })
    }

    /// An integer literal, checked against the spelling expected of it.
    ///
    /// A literal adapts to any written width, so this is the one place a
    /// literal too large for its slot can be refused at compile time rather
    /// than trapping when the value is finally narrowed.
    pub(crate) fn int_literal(
        &mut self,
        value: i64,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        // A hexadecimal literal is a bit pattern, and the one way a literal
        // can be negative: written into a `U64`, it names the unsigned value
        // of those bits.
        let denoted = if matches!(expected, Some(Type::Int(IntSpelling::U64))) && value < 0 {
            i128::from(value as u64)
        } else {
            i128::from(value)
        };
        if let Some(Type::Int(spelling)) = expected
            && !spelling.holds(denoted)
        {
            self.emit(
                span,
                "KSEM350",
                format!("the literal `{value}` does not fit `{}`", spelling.name()),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.program.exprs.alloc(HirExpr::Int(value))
    }
}
