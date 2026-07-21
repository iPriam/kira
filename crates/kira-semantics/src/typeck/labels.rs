//! Binding labeled call arguments to the callee's parameters.
//!
//! Split out of [`super::calls`] because it answers one self-contained
//! question: given the arguments a call wrote and the names its callee
//! declared, which value fills each parameter slot? A labeled argument
//! (`f(index: x)`, `f(index = x)`) names the parameter it binds, so the answer
//! is a permutation of the written arguments into declaration order — the same
//! positional list an unlabeled call already produces, which is why nothing
//! below this point ever learns a label was written.
//!
//! Kira's binder is unified with a struct literal's: `=` is canonical, `:`
//! stays valid for the transition window, and both reach here as one node. A
//! label is the parameter's own name, checked against the declaration; an
//! unknown one, a duplicate, a missing parameter, and a call that mixes
//! labeled and positional arguments each land a typed refusal.

use kira_core::Symbol;
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId};

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// Resolves `args` into positional order for a call to `name`.
    ///
    /// `leading` is the number of parameter slots already filled ahead of the
    /// written arguments — a method's receiver occupies slot 0 — so labels are
    /// matched against `param_names[leading..]`. `param_names` carries `None`
    /// for a receiver slot, which no label can name.
    ///
    /// Each entry is the value bound to one parameter slot, in declaration
    /// order, or `None` for a slot no argument filled. The ownership and type
    /// checks downstream see the same positional shape an unlabeled call
    /// produces; an unfilled slot is a missing argument, already reported here,
    /// that the caller stands in for so no second, vaguer arity error follows.
    /// An all-positional call keeps its written order untouched, as does the
    /// recovery from a structural mistake — an unknown or duplicate label, or a
    /// mix of labeled and positional arguments — because the program is already
    /// rejected and its remaining arguments should still be checked where they
    /// were written.
    pub(crate) fn bind_call_arguments(
        &mut self,
        args: &[CallArg],
        leading: usize,
        param_names: &[Option<Symbol>],
        name: &str,
        span: Span,
    ) -> Vec<Option<ExprId>> {
        let labeled = args.iter().filter(|arg| arg.label.is_some()).count();
        if labeled == 0 {
            return args.iter().map(|arg| Some(arg.value)).collect();
        }
        if labeled != args.len() {
            // The first positional argument is the one that reads as the
            // mistake once the call has committed to labels.
            let culprit = args
                .iter()
                .find(|arg| arg.label.is_none())
                .map_or(span, |arg| arg.span);
            self.emit(
                culprit,
                "KSEM189",
                format!(
                    "call to `{name}` mixes labeled and positional arguments; label every \
                     argument or none"
                ),
            );
            return args.iter().map(|arg| Some(arg.value)).collect();
        }

        // Every argument is labeled. Match each to the parameter slot whose name
        // it names, refusing an unknown or duplicated label. The receiver slots
        // are skipped: `param_names[..leading]` are `None` and unnameable.
        let written = &param_names[leading.min(param_names.len())..];
        let mut bound: Vec<Option<ExprId>> = vec![None; written.len()];
        let mut ok = true;
        for arg in args {
            let Some(label) = arg.label else {
                continue;
            };
            let label_span = arg.label_span.unwrap_or(arg.span);
            match written.iter().position(|slot| *slot == Some(label)) {
                Some(index) if bound[index].is_some() => {
                    ok = false;
                    self.emit(
                        label_span,
                        "KSEM188",
                        format!(
                            "duplicate argument label `{}` in call to `{name}`",
                            self.interner.resolve(label)
                        ),
                    );
                }
                Some(index) => bound[index] = Some(arg.value),
                None => {
                    ok = false;
                    self.emit(
                        label_span,
                        "KSEM187",
                        format!(
                            "`{name}` has no parameter named `{}`",
                            self.interner.resolve(label)
                        ),
                    );
                }
            }
        }
        if !ok {
            // A bad label already spoke; keep the written order so the values
            // still type-check where they were written rather than piling a
            // spurious arity error on top.
            return args.iter().map(|arg| Some(arg.value)).collect();
        }
        // A slot left unfilled is a missing argument, named by its parameter.
        // The `None` is kept so the caller fills the slot rather than letting an
        // arity mismatch re-report the same shortfall without the name.
        for (index, slot) in bound.iter().enumerate() {
            if slot.is_none()
                && let Some(param) = written[index]
            {
                self.emit(
                    span,
                    "KSEM190",
                    format!(
                        "call to `{name}` is missing an argument for parameter `{}`",
                        self.interner.resolve(param)
                    ),
                );
            }
        }
        bound
    }

    /// Reports labeled arguments on a call surface that binds no parameter
    /// names, keeping the values so they still type-check.
    ///
    /// A call through a function value, a `print`, a foreign function, or a
    /// class constructor exposes no named parameters to bind against here, so a
    /// label on one is a mistake rather than a binder. `surface` names the form
    /// for the message.
    pub(crate) fn reject_argument_labels(&mut self, args: &[CallArg], surface: &str) {
        for arg in args {
            if let Some(span) = arg.label_span {
                self.emit(
                    span,
                    "KSEM191",
                    format!("{surface} does not take argument labels"),
                );
            }
        }
    }

    /// The value expressions of `args`, in written order, dropping any labels.
    ///
    /// Used by a surface that does not bind labels, after
    /// [`Self::reject_argument_labels`] has reported them.
    pub(crate) fn argument_values(args: &[CallArg]) -> Vec<ExprId> {
        args.iter().map(|arg| arg.value).collect()
    }
}
