//! Call and construction type checking.
//!
//! The implementation is divided by call shape while shared resolution types
//! stay here. This keeps method, struct-literal, and ordinary-call paths
//! cohesive without changing the analyzer API.

mod construct;
mod dispatch;
mod user;

use kira_semantics_model::Type;
use kira_semantics_model::hir::FuncId;
use kira_syntax_model::ast::{CallArg, ExprId};

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
}
