//! The one description of a callable's contract.
//!
//! Every way of calling a function — an ordinary call, a trait requirement
//! and the implementation that satisfies it, a function value, a task target,
//! a foreign callback, a generated dispatcher — answers the same questions:
//! what the receiver is and whether it may be written, what each parameter
//! takes and in which ownership mode, whether it has a default and what it
//! is labelled, what comes back, whether the body is a task entry point, and
//! which thread may run it. They are answered here, once, so no phase compares
//! types alone where the contract says more.

use kira_runtime_abi::Execution;
use kira_syntax_model::ownership::OwnershipMode;

use crate::Type;

/// Which thread a callable may run on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ThreadAffinity {
    /// Any thread the runtime schedules it on.
    Any,
    /// The main thread only (`@MainThread`, or a lifecycle entry).
    MainThread,
}

/// The receiver a method runs on.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReceiverSignature {
    /// The receiver's type.
    pub ty: Type,
    /// Whether the body may write through the receiver (`borrow mut self`).
    pub mutable: bool,
}

/// One parameter of a callable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ParamSignature {
    /// The parameter's label, as declared.
    pub label: String,
    /// The type the parameter takes.
    pub ty: Type,
    /// How the argument is passed: owned, moved, copied, or borrowed.
    pub ownership: OwnershipMode,
    /// Whether a call may leave the argument out.
    pub has_default: bool,
}

/// The complete contract of one callable.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CallableSignature {
    /// The receiver, for a method; `None` for a free function.
    pub receiver: Option<ReceiverSignature>,
    /// The parameters, in declaration order, receiver excluded.
    pub params: Vec<ParamSignature>,
    /// The result type. A result is always owned by the caller.
    pub result: Type,
    /// Whether the body is a task entry point (`async function`).
    pub is_async: bool,
    /// Which thread may run the body.
    pub affinity: ThreadAffinity,
    /// Which engine runs the body.
    pub execution: Execution,
}

impl CallableSignature {
    /// The contract of a compiler-synthesized function: owned, unlabelled
    /// parameters of the given types, no receiver, a caller-owned result,
    /// runnable on any thread and any engine.
    #[must_use]
    pub fn synthesized(params: &[Type], result: Type) -> Self {
        Self {
            receiver: None,
            params: params
                .iter()
                .map(|&ty| ParamSignature {
                    label: String::new(),
                    ty,
                    ownership: OwnershipMode::Owned,
                    has_default: false,
                })
                .collect(),
            result,
            is_async: false,
            affinity: ThreadAffinity::Any,
            execution: Execution::Inherited,
        }
    }

    /// The contract of a synthesized dispatcher: a receiver of `receiver`,
    /// then owned, unlabelled parameters of the given types.
    #[must_use]
    pub fn dispatcher(receiver: Type, mutable: bool, params: &[Type], result: Type) -> Self {
        Self {
            receiver: Some(ReceiverSignature {
                ty: receiver,
                mutable,
            }),
            ..Self::synthesized(params, result)
        }
    }

    /// The parameter types, receiver excluded.
    #[must_use]
    pub fn param_types(&self) -> Vec<Type> {
        self.params.iter().map(|param| param.ty).collect()
    }

    /// Whether any parameter is a borrow, read or mutable.
    #[must_use]
    pub fn borrows_any_parameter(&self) -> bool {
        self.params.iter().any(|param| param.ownership.is_borrow())
    }

    /// How `self` (an implementation) departs from `required` (a contract),
    /// as sentences a diagnostic can list; empty when the two agree on
    /// everything but the receiver's type, which the caller compares.
    ///
    /// Types are compared by the caller as well, so what is listed here is
    /// exactly what a type-only comparison misses: ownership modes, labels,
    /// defaults, receiver mutability, `async`, and thread affinity.
    #[must_use]
    pub fn contract_differences(&self, required: &CallableSignature) -> Vec<String> {
        let mut differences = Vec::new();
        match (&self.receiver, &required.receiver) {
            (Some(mine), Some(theirs)) if mine.mutable != theirs.mutable => {
                differences.push(format!(
                    "the receiver is `{}` where the contract says `{}`",
                    receiver_spelling(mine.mutable),
                    receiver_spelling(theirs.mutable)
                ));
            }
            _ => {}
        }
        for (index, (mine, theirs)) in self.params.iter().zip(&required.params).enumerate() {
            if mine.ownership != theirs.ownership {
                differences.push(format!(
                    "parameter {} is passed `{}` where the contract says `{}`",
                    index + 1,
                    ownership_spelling(mine.ownership),
                    ownership_spelling(theirs.ownership)
                ));
            }
            if mine.label != theirs.label {
                differences.push(format!(
                    "parameter {} is labelled `{}` where the contract says `{}`",
                    index + 1,
                    mine.label,
                    theirs.label
                ));
            }
            if mine.has_default != theirs.has_default {
                differences.push(format!(
                    "parameter {} {} a default where the contract {}",
                    index + 1,
                    if mine.has_default { "has" } else { "lacks" },
                    if theirs.has_default {
                        "has one"
                    } else {
                        "has none"
                    }
                ));
            }
        }
        if self.is_async != required.is_async {
            differences.push(if self.is_async {
                "it is `async` where the contract is not".to_owned()
            } else {
                "it is not `async` where the contract is".to_owned()
            });
        }
        if self.affinity != required.affinity {
            differences.push(match self.affinity {
                ThreadAffinity::MainThread => {
                    "it is `@MainThread` where the contract is not".to_owned()
                }
                ThreadAffinity::Any => "it is not `@MainThread` where the contract is".to_owned(),
            });
        }
        differences
    }
}

fn receiver_spelling(mutable: bool) -> &'static str {
    if mutable {
        "borrow mut self"
    } else {
        "borrow self"
    }
}

fn ownership_spelling(mode: OwnershipMode) -> &'static str {
    match mode {
        OwnershipMode::Owned => "owned",
        OwnershipMode::BorrowRead => "borrow",
        OwnershipMode::BorrowMut => "borrow mut",
        OwnershipMode::Move => "move",
        OwnershipMode::Copy => "copy",
    }
}
