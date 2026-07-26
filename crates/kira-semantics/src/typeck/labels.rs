//! Binding a call's written arguments to the callee's parameter slots.
//!
//! **A label on a call argument is decorative.** `f(width = 2.0)` binds the
//! same slot `f(2.0)` does — the label documents which parameter the reader is
//! looking at and has no effect on where the value lands. So a call may label
//! some arguments and not others, may label them all, or none, and means the
//! same thing every time.
//!
//! This is measured behaviour, not a simplification. The reference
//! implementation binds a call positionally and ignores labels entirely:
//! `tag(c = 3, b = 2, a = 1)` calls `tag(3, 2, 1)` there. Resolving labels to
//! parameter names instead — which this compiler did until the two were run
//! against each other — makes that program mean something different without
//! either compiler complaining, which is the one failure a differential exists
//! to catch.
//!
//! A **construction** is the exception, and it is not one: `Widget(b = 2, a = 1)`
//! binds by name, because a construction's inputs are fields and a field
//! initializer names its field. That path is [`crate::constructs`], not this
//! one, and both implementations agree on it.

use kira_syntax_model::ast::{CallArg, ExprId};

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// The written arguments as parameter slots, in order, labels dropped.
    ///
    /// Every entry is `Some`: a written argument always fills the next slot.
    /// The `Option` is the caller's vocabulary for a slot no argument reached,
    /// which it fills from that parameter's default.
    pub(crate) fn argument_slots(args: &[CallArg]) -> Vec<Option<ExprId>> {
        args.iter().map(|arg| Some(arg.value)).collect()
    }

    /// The value expressions of `args`, in written order, dropping any labels.
    pub(crate) fn argument_values(args: &[CallArg]) -> Vec<ExprId> {
        args.iter().map(|arg| arg.value).collect()
    }
}
