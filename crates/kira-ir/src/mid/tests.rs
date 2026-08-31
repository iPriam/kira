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
        is_main_thread: false,
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
    user_drop: BorrowLending::ByPointer,
};

/// An empty type table: every type these tests use answers `owns_heap`
/// from its own shape, with no struct or enum declaration to look up.
fn types() -> TypeTable {
    TypeTable::default()
}

#[test]
fn only_the_slots_that_own_storage_are_released() {
    let function = function(vec![Type::INT, Type::String, Type::Bool, Type::String]);
    let plan = plan_function(&function, &types(), BY_VALUE, false).expect("a plan");
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
    let plan = plan_function(&function, &types(), BY_POINTER, false).expect("a plan");
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
    let plan = plan_function(&function, &types(), BY_VALUE, false).expect("a plan");
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
        user_drop: BorrowLending::ByPointer,
    };
    let plan = plan_function(&function, &types(), mixed, false).expect("a plan");
    assert_eq!(plan.slots(), &[0, 2]);
}

/// A callback-state local names a value in a store that outlives the call.
#[test]
fn a_callback_state_local_is_left_to_its_store() {
    let mut function = function(vec![Type::String, Type::String]);
    function.native_state_locals = vec![None, Some(kira_runtime_abi::NativeStateTypeId::new(0))];
    let plan = plan_function(&function, &types(), BY_VALUE, false).expect("a plan");
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
        plan_function(&function, &types(), BY_VALUE, false),
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
        plan_function(&function, &types(), BY_VALUE, false),
        Err(MidError::UnknownParameter { slot: 7, .. })
    ));
}

/// The plan is ascending, which is what makes `releases` a binary search
/// and what keeps two engines from releasing in two different orders.
#[test]
fn the_plan_is_in_slot_order() {
    let function = function(vec![Type::String; 5]);
    let plan = plan_function(&function, &types(), BY_VALUE, false).expect("a plan");
    assert_eq!(plan.slots(), &[0, 1, 2, 3, 4]);
    assert!(plan.slots().windows(2).all(|pair| pair[0] < pair[1]));
}
