//! Choosing among the declarations that share a name.
//!
//! Several declarations may answer to one name as long as they take different
//! things. A call picks the one its arguments fit, and the rule is deliberately
//! small enough to hold in your head:
//!
//! 1. **Arity.** A candidate is in the running when the call passes no more
//!    arguments than it takes and every slot the call left empty declares a
//!    default.
//! 2. **Admissibility.** Every argument must be accepted by its parameter, by
//!    the same rule a non-overloaded call is checked with.
//! 3. **Specificity.** Among the survivors, an argument whose type *is* the
//!    parameter's beats one that has to convert or erase into it. The candidate
//!    with the fewest such conversions wins.
//! 4. **Directness.** Between two candidates that convert equally, the one that
//!    fills fewer slots from defaults wins, so `f(Int)` beats
//!    `f(Int, String = "x")` for `f(1)`.
//!
//! A tie at step 4 is ambiguous and is reported rather than broken by
//! declaration order: a program whose meaning depends on which file was read
//! first is a program whose meaning nobody can read off the source.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{FuncId, HirExprId};
use kira_syntax_model::ast::CallArg;

use crate::analyze::{Analyzer, FnCtx};

/// Why a call matched no single declaration of an overloaded name.
pub(crate) enum OverloadFailure {
    /// No candidate accepts these arguments.
    None,
    /// Several accept them equally well.
    Ambiguous(Vec<FuncId>),
}

impl Analyzer<'_> {
    /// Picks the declaration of `name` a call with these argument types means.
    ///
    /// `Ok` for a name declared once, which is every name in a program that
    /// overloads nothing: the single candidate is returned without ranking, so
    /// its arity and type errors stay exactly the ones it reported before.
    pub(crate) fn resolve_overload(
        &mut self,
        candidates: &[FuncId],
        args: &[Type],
    ) -> Result<FuncId, OverloadFailure> {
        if let [only] = candidates {
            return Ok(*only);
        }
        let mut best: Vec<(FuncId, (u32, u32))> = Vec::new();
        for &id in candidates {
            let Some(score) = self.overload_score(id, args) else {
                continue;
            };
            best.push((id, score));
        }
        let Some(top) = best.iter().map(|(_, score)| *score).min() else {
            return Err(OverloadFailure::None);
        };
        let winners: Vec<FuncId> = best
            .iter()
            .filter(|(_, score)| *score == top)
            .map(|(id, _)| *id)
            .collect();
        match winners.as_slice() {
            [only] => Ok(*only),
            _ => Err(OverloadFailure::Ambiguous(winners)),
        }
    }

    /// How badly `args` fit `id`, or `None` when they do not fit at all.
    ///
    /// The first number counts arguments that reach their parameter by a
    /// conversion rather than by being it already — a converted integer, a
    /// concrete declaration erased into `Any Family`, a subclass passed as its
    /// parent. The second counts slots filled from defaults. Lower is a closer
    /// fit in that order, and `(0, 0)` is an exact one.
    pub(crate) fn overload_score(&mut self, id: FuncId, args: &[Type]) -> Option<(u32, u32)> {
        let params = self.param_types(id);
        if args.len() > params.len() {
            return None;
        }
        // A slot the call left empty is filled from its default, so a candidate
        // that declares one for every missing slot still fits.
        for slot in args.len()..params.len() {
            self.param_default(id, slot)?;
        }
        let mut conversions = 0;
        for (&actual, &expected) in args.iter().zip(params.iter()) {
            if !self.argument_reaches(actual, expected) {
                return None;
            }
            if actual != expected {
                conversions += 1;
            }
        }
        Some((conversions, (params.len() - args.len()) as u32))
    }

    /// Whether an argument of type `actual` can reach a parameter of type
    /// `expected`.
    ///
    /// Wider than [`Analyzer::admits_argument`] by one crossing: erasing a
    /// construct-backed declaration into a family it backs. That crossing is
    /// performed by the expectation rather than by the lattice, so a checker
    /// looking at the argument *before* it is analyzed against the parameter
    /// has to ask for it explicitly — and overload resolution is exactly that
    /// checker.
    pub(crate) fn argument_reaches(&self, actual: Type, expected: Type) -> bool {
        if self.admits_argument(actual, expected) {
            return true;
        }
        let (Type::Struct(id), Type::Enum(family)) = (actual, expected) else {
            return false;
        };
        self.is_construct_family_type(family)
            && self
                .constructs
                .get(&id)
                .is_some_and(|info| info.families.iter().any(|(enum_id, _)| *enum_id == family))
    }

    /// The types a call's written arguments have, computed without keeping
    /// anything the computing did.
    ///
    /// Overload resolution has a genuine ordering problem: which declaration is
    /// called decides how each argument is analyzed — what type is expected of
    /// it, whether its ownership mode consumes the local it names — and the
    /// arguments' types decide which declaration is called. This breaks the
    /// cycle by analyzing them once against no expectation, reading the types
    /// off, and then throwing the analysis away: the frame is a copy, and the
    /// diagnostics and definition links the trial produced are rolled back. The
    /// real analysis then runs against the declaration that won, and it is the
    /// only one that reports anything.
    ///
    /// Only an overloaded name pays for this. Everything else takes the single
    /// candidate and analyzes its arguments once.
    pub(crate) fn try_argument_types(
        &mut self,
        ctx: &FnCtx,
        leading: &[HirExprId],
        args: &[CallArg],
    ) -> Vec<Type> {
        let diagnostics = self.diagnostics.len();
        let definitions = self.definitions.len();
        let mut trial = ctx.clone();
        let mut types: Vec<Type> = leading
            .iter()
            .map(|&value| self.program.expr(value).type_of())
            .collect();
        for arg in args {
            let value = self.analyze_expr(&mut trial, arg.value);
            types.push(self.program.expr(value).type_of());
        }
        self.diagnostics.truncate(diagnostics);
        self.definitions.truncate(definitions);
        types
    }

    /// Names every candidate for the ambiguity diagnostic.
    pub(crate) fn overload_list(&self, ids: &[FuncId]) -> String {
        let written: Vec<String> = ids
            .iter()
            .map(|&id| {
                let (params, result) = self.signature_of(id);
                let params: Vec<String> = params.iter().map(|&ty| self.type_name(ty)).collect();
                format!("({}) -> {}", params.join(", "), self.type_name(result))
            })
            .collect();
        written.join(", ")
    }
}
