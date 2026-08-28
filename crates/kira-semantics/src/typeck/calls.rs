//! Calls and construction: every expression that names something and hands it
//! arguments.
//!
//! Split out of [`super`] because it is a cohesive surface with one shared
//! question — *what is being called, and does the argument list fit its
//! signature* — and because four of the five kinds here share the argument
//! checking that [`Analyzer::analyze_user_call`] does. A method call, a
//! module-qualified call, and a bare call all end up there; a struct literal
//! and `print` are the two that do not, and they are the two that are not
//! calls to a user function.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{
    Callee, FuncId, HirExpr, HirExprId, HirPlace, HirWriteback, LocalId,
};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};
use crate::place::PlacePurpose;

#[path = "calls/arguments.rs"]
mod arguments;
#[path = "calls/literal.rs"]
mod literal;
#[path = "calls/math.rs"]
mod math;
#[path = "calls/methods.rs"]
mod methods;
#[path = "calls/targets.rs"]
mod targets;

/// Written argument and child lists of one method call.
pub(super) struct MethodCallContent<'a> {
    pub(super) args: &'a [CallArg],
    pub(super) children: &'a [ExprId],
}

/// What a written call name resolved to.
enum CallTarget {
    /// The declaration the call means, and the parameter types its arguments
    /// are checked against.
    Chosen(FuncId, Vec<Type>),
    /// Several declarations fit the call equally well.
    Ambiguous(Vec<FuncId>),
    /// Nothing answers to the name here.
    Unknown,
    /// The name was a generic function, but its explicit or inferred type
    /// arguments were invalid. Its own diagnostic is sufficient; do not add a
    /// cascading undefined-function error.
    Invalid,
}

/// The syntax and context needed to type-check one user call.
pub(super) struct CallSyntax<'a> {
    /// The function context receiving ownership effects.
    pub(super) ctx: &'a mut FnCtx,
    /// The written callee name.
    pub(super) name: &'a str,
    /// Already analyzed leading values, such as a method receiver.
    pub(super) leading: &'a [HirExprId],
    /// Explicit generic type arguments.
    pub(super) type_args: &'a [TypeRefId],
    /// Written arguments.
    pub(super) args: &'a [CallArg],
    /// Already analyzed trailing values, such as child content.
    pub(super) trailing: &'a [HirExprId],
    /// Source span of the call.
    pub(super) span: Span,
}

impl Analyzer<'_> {
    /// Records a receiver-writeback place on a just-built call whose callee
    /// mutates its receiver, resolving the written `receiver` as a mutable
    /// place.
    ///
    /// A non-mutating callee — the common case — is left untouched, so an
    /// ordinary call carries no writeback and behaves exactly as before. A
    /// receiver that is not a mutable place turns the call into an error, the
    /// diagnostic already reported by place resolution (`KSEM021` for an
    /// immutable binding, `KSEM211` for a temporary).
    pub(crate) fn record_mut_receiver(
        &mut self,
        ctx: &mut FnCtx,
        call: HirExprId,
        receiver: ExprId,
    ) {
        if !self.callee_mutates(call) {
            return;
        }
        match self.resolve_place(ctx, receiver, PlacePurpose::MutCall) {
            Some((place, _)) => self.add_writeback(call, HirWriteback { param: 0, place }),
            None => self.program.exprs[call] = HirExpr::Error,
        }
    }

    /// Records the writeback for a call whose receiver is `self` — an implicit
    /// or parent-qualified call inside a method.
    ///
    /// `self` inside a mutating method is always a mutable place (the fixpoint
    /// marks the enclosing method mutating whenever it calls one on `self`), so
    /// the place is `self` with an empty path and no refusal is possible.
    pub(crate) fn record_mut_self(&mut self, call: HirExprId, self_local: LocalId) {
        if !self.callee_mutates(call) {
            return;
        }
        self.add_writeback(
            call,
            HirWriteback {
                param: 0,
                place: HirPlace {
                    local: self_local,
                    path: Vec::new(),
                },
            },
        );
    }

    /// Whether `call` is a real user call whose chosen callee mutates its
    /// receiver.
    ///
    /// The question is asked of the callee *the call resolved to* — overload
    /// resolution already picked the right declaration for this receiver's
    /// type — rather than of a display name looked up again: two packages may
    /// each declare a method of one name under one index key, and re-asking
    /// by name can read the other package's flag, losing or inventing a
    /// writeback.
    fn callee_mutates(&self, call: HirExprId) -> bool {
        let HirExpr::Call {
            callee: Callee::User(id),
            ..
        } = self.program.expr(call)
        else {
            return false;
        };
        self.mutates_self(*id)
    }

    /// Adds a writeback to a call node, keeping the list in parameter order.
    ///
    /// A method call records its receiver here *after* the call node exists,
    /// while a `borrow mut` argument records its own slot while the arguments
    /// are still being bound, so the two arrive out of order and the list is
    /// kept sorted rather than appended to. A slot already recorded is left
    /// alone: it names the same place, and a second entry would write it twice.
    fn add_writeback(&mut self, call: HirExprId, writeback: HirWriteback) {
        if let HirExpr::Call { writebacks, .. } = &mut self.program.exprs[call] {
            match writebacks.binary_search_by_key(&writeback.param, |entry| entry.param) {
                Ok(_) => {}
                Err(index) => writebacks.insert(index, writeback),
            }
        }
    }
}
