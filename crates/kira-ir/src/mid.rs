//! The mid-level stage decides release ownership once for every backend.
//!
//! Lowering records one release plan for each owned local. Reading a local
//! copies its heap value, so one release per slot at return is sufficient.
//! Borrowed parameters and callback-state locals are excluded because their
//! storage belongs to the caller or to the state store.
//!
//! Whether a borrow is represented by a pointer is engine-specific: native code
//! lends the caller's storage, while the VM copies the value into the callee's
//! slot and moves it back. The plan therefore receives [`Lending`] from
//! lowering rather than deriving it in either backend. So is what a slot's
//! death has to free: the native frame lays a scalar-only struct out inline
//! while the VM boxes every struct, so planning receives [`HeapModel`] too.
//! Each backend builds its plan with its own lending and model; a plan built
//! under one engine's pair is not the other's.
//!
//! Ownership has a second half: *when*. [`scope_releases`] walks each body and
//! places a [`IrStmt::ReleaseLocals`] wherever a block-scoped binding dies —
//! the end of its declaring block, and before every `break`/`continue` that
//! jumps past it. Placement asks only which bindings a block declares, which
//! no engine disagrees about; whether a named slot is this engine's to release
//! stays with the plan, which each backend consults when it lowers the
//! statement.

use kira_semantics_model::{Type, TypeTable};

use crate::ir::{IrFunction, IrProgram};

/// Why a release plan could not be built.
///
/// Each variant is a contradiction *within one function* — two facts that
/// cannot both be true of the same slot. They are compiler bugs rather than
/// program errors: nothing a user can write reaches one, because every input
/// here was resolved by lowering.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MidError {
    /// A slot is both a by-reference parameter and a callback-state local.
    ///
    /// The two say opposite things about who owns the storage — the caller, or
    /// a store outside the call — and a slot cannot be both.
    #[error(
        "function `{function}` slot {slot} is both a by-reference parameter and a \
         callback-state local, which name different owners"
    )]
    ConflictingSlotRole {
        /// The function the slot belongs to.
        function: String,
        /// The slot in question.
        slot: u32,
    },
    /// A by-reference parameter names a slot the function does not have.
    ///
    /// Left as an error rather than ignored: a parameter index that resolves to
    /// nothing means lowering and this stage disagree about how many locals the
    /// function has, and guessing which is right would release the wrong slot.
    #[error("function `{function}` names by-reference parameter {slot}, which is not a local")]
    UnknownParameter {
        /// The function the parameter belongs to.
        function: String,
        /// The slot the parameter named.
        slot: u32,
    },
}

/// Which slots one function releases when it returns, in slot order.
///
/// Slot order rather than declaration or reverse order: nothing in the language
/// observes the order releases happen in — a release touches only the value's
/// own storage — and a fixed order is one fewer thing for two engines to
/// disagree about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReleasePlan {
    slots: Vec<u32>,
}

impl ReleasePlan {
    /// The slots to release, ascending.
    pub fn slots(&self) -> &[u32] {
        &self.slots
    }

    /// Whether `slot` is released by this plan.
    pub fn releases(&self, slot: u32) -> bool {
        self.slots.binary_search(&slot).is_ok()
    }

    /// How many slots the plan releases.
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether the plan releases nothing, which is the common case.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

/// Which memory layout an engine gives the values a frame holds, which is
/// what decides whether a slot's death has anything to release.
///
/// The two engines answer differently for structs and for nothing else. A
/// native frame lays a scalar-only struct out inline, so its death frees
/// nothing; the VM allocates a heap object for *every* struct, so its death
/// frees one however scalar its fields are. A plan built for one model and
/// consumed under the other either leaks (inline plan, boxed heap) or
/// releases storage that was never allocated (boxed plan, inline heap) — so
/// the model is an input to planning, exactly as [`Lending`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeapModel {
    /// Aggregates without heap-owning fields live inline in the frame.
    Inline,
    /// Every struct is a heap object, whatever its fields hold.
    Boxed,
}

impl HeapModel {
    /// Whether a slot of `ty` owns storage this engine must release.
    pub fn owns(self, types: &TypeTable, ty: Type) -> bool {
        match self {
            HeapModel::Inline => types.owns_heap(ty),
            HeapModel::Boxed => types.owns_heap(ty) || matches!(ty, Type::Struct(_)),
        }
    }
}

/// Whether a borrowed parameter reaches a callee as a pointer into the
/// caller's storage, or as a value of its own.
///
/// A parameter of the [`BorrowLending::ByPointer`] kind is the caller's to
/// release; one of the [`BorrowLending::ByValue`] kind arrived as a copy the
/// callee owns and must release itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorrowLending {
    /// The borrow is a pointer into the caller's storage.
    ByPointer,
    /// The borrow arrived as the callee's own value.
    ByValue,
}

/// How an engine's calls lend the two kinds of borrowed parameter.
///
/// Two fields rather than one because the two kinds are lent independently.
/// Whether a `borrow mut` is a pointer is fixed per engine — the native backend
/// always passes one, the VM never can. Whether a plain `borrow` is a pointer
/// varies by module shape even within the native backend, since lending one
/// commits every call site and only a module that compiles all of them may
/// decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lending {
    /// How a `borrow` parameter arrives.
    pub read_only: BorrowLending,
    /// How a `borrow mut` parameter arrives.
    pub write_through: BorrowLending,
    /// How a `borrow` of a type that runs a user `Drop` arrives.
    ///
    /// Its own field because the two engines answer differently for a reason
    /// neither of the others carries. A VM copy is a share of one object, so a
    /// borrowed copy releases a share and runs no body; a native copy is a
    /// second value, and releasing it would run the body while the caller still
    /// holds what it was told about. So native lends one however it lends
    /// everything else, and the VM copies it however it copies everything else.
    pub user_drop: BorrowLending,
}

impl Lending {
    /// Every borrow arrives as the callee's own value, which is the VM's only
    /// option and what a native module that lends nothing does too.
    pub const BY_VALUE: Lending = Lending {
        read_only: BorrowLending::ByValue,
        write_through: BorrowLending::ByValue,
        user_drop: BorrowLending::ByValue,
    };
}

/// Builds the release plan for one function.
///
/// `drop_glue` says this function is the body of a type's user `Drop`. Its
/// receiver is then excluded: the storage belongs to whatever is releasing the
/// value, which releases the members itself once the body has run. Releasing it
/// here would re-enter the same body on the same value.
pub fn plan_function(
    function: &IrFunction,
    types: &TypeTable,
    lending: Lending,
    model: HeapModel,
    drop_glue: bool,
) -> Result<ReleasePlan, MidError> {
    let local_count = function.locals.len();
    for &slot in &function.by_reference_params {
        if slot as usize >= local_count {
            return Err(MidError::UnknownParameter {
                function: function.name.clone(),
                slot,
            });
        }
    }

    let mut slots = Vec::new();
    for (index, &ty) in function.locals.iter().enumerate() {
        let slot = index as u32;
        let written_through = function.by_reference_params.contains(&slot);
        let borrowed_drop = types.runs_user_drop(ty);
        let lent = match (written_through, function.by_pointer_params.contains(&slot)) {
            (true, _) => lending.write_through == BorrowLending::ByPointer,
            (false, true) if borrowed_drop => lending.user_drop == BorrowLending::ByPointer,
            (false, read_only) => read_only && lending.read_only == BorrowLending::ByPointer,
        };
        let state_local = function
            .native_state_locals
            .get(index)
            .copied()
            .flatten()
            .is_some();
        // The contradiction is in the function, not in this engine's lending:
        // a slot that is a parameter at all cannot also name a store outside
        // the call, however that parameter happens to arrive.
        if (written_through || function.by_pointer_params.contains(&slot)) && state_local {
            return Err(MidError::ConflictingSlotRole {
                function: function.name.clone(),
                slot,
            });
        }
        if lent || state_local || !model.owns(types, ty) {
            continue;
        }
        if drop_glue && slot == 0 {
            continue;
        }
        slots.push(slot);
    }
    Ok(ReleasePlan { slots })
}

/// Builds a release plan for every function in `program`, in function order.
pub fn plan(
    program: &IrProgram,
    lending: Lending,
    model: HeapModel,
) -> Result<Vec<ReleasePlan>, MidError> {
    let glue: std::collections::BTreeSet<u32> = program
        .types
        .structs()
        .defs()
        .iter()
        .filter_map(|def| def.drop_glue)
        .collect();
    program
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            plan_function(
                function,
                &program.types,
                lending,
                model,
                glue.contains(&(index as u32)),
            )
        })
        .collect()
}

mod scope;
pub use scope::scope_releases;

#[cfg(test)]
mod tests;
