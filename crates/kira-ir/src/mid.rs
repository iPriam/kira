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
//! lowering rather than deriving it in either backend. The bytecode compiler
//! serializes the same plan that LLVM consumes while emitting a `return`.

use kira_semantics_model::TypeTable;

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
}

impl Lending {
    /// Every borrow arrives as the callee's own value, which is the VM's only
    /// option and what a native module that lends nothing does too.
    pub const BY_VALUE: Lending = Lending {
        read_only: BorrowLending::ByValue,
        write_through: BorrowLending::ByValue,
    };
}

/// Builds the release plan for one function.
pub fn plan_function(
    function: &IrFunction,
    types: &TypeTable,
    lending: Lending,
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
        let lent = match (written_through, function.by_pointer_params.contains(&slot)) {
            (true, _) => lending.write_through == BorrowLending::ByPointer,
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
        if lent || state_local || !types.owns_heap(ty) {
            continue;
        }
        slots.push(slot);
    }
    Ok(ReleasePlan { slots })
}

/// Builds a release plan for every function in `program`, in function order.
pub fn plan(program: &IrProgram, lending: Lending) -> Result<Vec<ReleasePlan>, MidError> {
    program
        .functions
        .iter()
        .map(|function| plan_function(function, &program.types, lending))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_runtime_abi::Execution;
    use kira_semantics_model::Type;

    fn function(locals: Vec<Type>) -> IrFunction {
        IrFunction {
            name: "probe".to_owned(),
            param_count: 0,
            locals,
            native_state_locals: Vec::new(),
            return_type: Type::Void,
            body: Vec::new(),
            execution: Execution::Runtime,
            by_reference_params: Vec::new(),
            by_pointer_params: Vec::new(),
        }
    }

    /// The lending most tests here do not depend on.
    const BY_VALUE: Lending = Lending::BY_VALUE;

    /// The native backend's: both kinds of borrow are the caller's storage.
    const BY_POINTER: Lending = Lending {
        read_only: BorrowLending::ByPointer,
        write_through: BorrowLending::ByPointer,
    };

    /// An empty type table: every type these tests use answers `owns_heap`
    /// from its own shape, with no struct or enum declaration to look up.
    fn types() -> TypeTable {
        TypeTable::default()
    }

    #[test]
    fn only_the_slots_that_own_storage_are_released() {
        let function = function(vec![Type::INT, Type::String, Type::Bool, Type::String]);
        let plan = plan_function(&function, &types(), BY_VALUE).expect("a plan");
        assert_eq!(plan.slots(), &[1, 3]);
        assert!(plan.releases(1));
        assert!(!plan.releases(0), "an integer has nothing to release");
    }

    /// A by-reference parameter lent by pointer is the caller's storage, and
    /// releasing it here would free a value the caller still holds and will
    /// free itself.
    #[test]
    fn a_by_reference_parameter_lent_by_pointer_is_left_to_its_caller() {
        let mut function = function(vec![Type::String, Type::String]);
        function.param_count = 1;
        function.by_reference_params = vec![0];
        let plan = plan_function(&function, &types(), BY_POINTER).expect("a plan");
        assert_eq!(plan.slots(), &[1]);
    }

    /// The same parameter on an engine that cannot lend a pointer. The callee
    /// holds a copy of its own — the caller kept the original and gets the
    /// copy back by writeback — so leaving the slot out would leak it.
    #[test]
    fn a_by_reference_parameter_passed_by_value_is_the_callee_s_to_release() {
        let mut function = function(vec![Type::String, Type::String]);
        function.param_count = 1;
        function.by_reference_params = vec![0];
        let plan = plan_function(&function, &types(), BY_VALUE).expect("a plan");
        assert_eq!(plan.slots(), &[0, 1]);
    }

    /// A read-only borrow is lent independently of a written-through one: the
    /// native backend lends both, a library lends neither, and no engine has
    /// ever needed the mixed case — but the two are separate inputs, so the
    /// plan answers each on its own rather than from whichever was asked last.
    #[test]
    fn each_kind_of_borrow_is_lent_on_its_own_terms() {
        let mut function = function(vec![Type::String, Type::String, Type::String]);
        function.param_count = 2;
        function.by_reference_params = vec![0];
        function.by_pointer_params = vec![1];
        let mixed = Lending {
            read_only: BorrowLending::ByPointer,
            write_through: BorrowLending::ByValue,
        };
        let plan = plan_function(&function, &types(), mixed).expect("a plan");
        assert_eq!(plan.slots(), &[0, 2]);
    }

    /// A callback-state local names a value in a store that outlives the call.
    #[test]
    fn a_callback_state_local_is_left_to_its_store() {
        let mut function = function(vec![Type::String, Type::String]);
        function.native_state_locals =
            vec![None, Some(kira_runtime_abi::NativeStateTypeId::new(0))];
        let plan = plan_function(&function, &types(), BY_VALUE).expect("a plan");
        assert_eq!(plan.slots(), &[0]);
    }

    /// Two facts that cannot both hold of one slot are a compiler bug, and are
    /// reported rather than resolved by preferring one of them.
    #[test]
    fn a_slot_cannot_be_both_borrowed_and_state_backed() {
        let mut function = function(vec![Type::String]);
        function.by_reference_params = vec![0];
        function.native_state_locals = vec![Some(kira_runtime_abi::NativeStateTypeId::new(0))];
        assert!(matches!(
            plan_function(&function, &types(), BY_VALUE),
            Err(MidError::ConflictingSlotRole { slot: 0, .. })
        ));
    }

    /// A parameter index that names no local means this stage and lowering
    /// disagree about the function's shape, which must not be guessed past.
    #[test]
    fn a_parameter_naming_no_local_is_refused() {
        let mut function = function(vec![Type::String]);
        function.by_reference_params = vec![7];
        assert!(matches!(
            plan_function(&function, &types(), BY_VALUE),
            Err(MidError::UnknownParameter { slot: 7, .. })
        ));
    }

    /// The plan is ascending, which is what makes `releases` a binary search
    /// and what keeps two engines from releasing in two different orders.
    #[test]
    fn the_plan_is_in_slot_order() {
        let function = function(vec![Type::String; 5]);
        let plan = plan_function(&function, &types(), BY_VALUE).expect("a plan");
        assert_eq!(plan.slots(), &[0, 1, 2, 3, 4]);
        assert!(plan.slots().windows(2).all(|pair| pair[0] < pair[1]));
    }
}
