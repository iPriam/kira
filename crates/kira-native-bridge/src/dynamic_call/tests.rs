//! Tests for the run-time call builder.
use super::*;

/// Stands in for a COM method: `this` first, then arguments, `HRESULT` back.
extern "system" fn sum_three(a: usize, b: usize, c: usize) -> i32 {
    (a + b + c) as i32
}

/// Nine arguments, past the four the Microsoft x64 convention passes in
/// registers, so the stack half of the arm is exercised too.
extern "system" fn sum_nine(
    a: usize,
    b: usize,
    c: usize,
    d: usize,
    e: usize,
    f: usize,
    g: usize,
    h: usize,
    i: usize,
) -> u64 {
    (a + b + c + d + e + f + g + h + i) as u64
}

extern "system" fn identity_ptr(value: *mut c_void) -> *mut c_void {
    value
}

extern "system" fn takes_nothing_returns_float() -> f32 {
    2.5
}

/// A vtable slot writer, standing in for `object->lpVtbl->Method`.
extern "system" fn add_one(value: usize) -> u64 {
    (value + 1) as u64
}

fn call_of(max_args: u32) -> *mut c_void {
    let call = kira_dynamic_call_new(max_args);
    assert!(!call.is_null());
    call
}

#[test]
fn three_integer_arguments_reach_the_callee() {
    let call = call_of(3);
    // SAFETY: the call is live, and the target takes three integer
    // arguments and returns an i32.
    unsafe {
        kira_dynamic_call_arg_u64(call, 10);
        kira_dynamic_call_arg_u64(call, 20);
        kira_dynamic_call_arg_u64(call, 12);
        let result = kira_dynamic_call_invoke_i32(call, sum_three as *mut c_void);
        assert_eq!(result, 42);
        assert_eq!(kira_dynamic_call_status(call), KIRA_DYNAMIC_CALL_OK);
        kira_dynamic_call_free(call);
    }
}

#[test]
fn arguments_past_the_register_four_land_on_the_stack_correctly() {
    let call = call_of(9);
    // SAFETY: the call is live, and the target takes nine integer
    // arguments and returns a u64.
    unsafe {
        for value in 1..=9u64 {
            kira_dynamic_call_arg_u64(call, value);
        }
        let result = kira_dynamic_call_invoke_u64(call, sum_nine as *mut c_void);
        assert_eq!(result, 45, "1..=9 summed, so no argument was dropped");
        kira_dynamic_call_free(call);
    }
}

#[test]
fn a_pointer_argument_returns_unchanged() {
    let call = call_of(1);
    let value = kira_dynamic_call_new(0);
    // SAFETY: the call is live, and the target takes and returns a pointer.
    unsafe {
        kira_dynamic_call_arg_ptr(call, value);
        let result = kira_dynamic_call_invoke_ptr(call, identity_ptr as *mut c_void);
        assert_eq!(result, value);
        kira_dynamic_call_free(value);
        kira_dynamic_call_free(call);
    }
}

#[test]
fn a_float_return_survives_an_integer_argument_list() {
    let call = call_of(0);
    // SAFETY: the call is live and the target takes nothing.
    unsafe {
        let result = kira_dynamic_call_invoke_f32(call, takes_nothing_returns_float as *mut c_void);
        assert_eq!(result.to_bits(), 2.5f32.to_bits());
        kira_dynamic_call_free(call);
    }
}

#[test]
fn a_float_argument_refuses_the_call_rather_than_misplacing_it() {
    let call = call_of(2);
    // SAFETY: the call is live; no invoke reaches the target here.
    unsafe {
        kira_dynamic_call_arg_u64(call, 1);
        kira_dynamic_call_arg_f32(call, 1.5);
        assert_eq!(
            kira_dynamic_call_status(call),
            KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED
        );
        let result = kira_dynamic_call_invoke_u64(call, add_one as *mut c_void);
        assert_eq!(result, 0, "the target must not have run");
        kira_dynamic_call_free(call);
    }
}

#[test]
fn pushing_past_the_declared_capacity_is_reported_not_grown() {
    let call = call_of(1);
    // SAFETY: the call is live; the invoke is refused before any target runs.
    unsafe {
        kira_dynamic_call_arg_u64(call, 1);
        kira_dynamic_call_arg_u64(call, 2);
        assert_eq!(
            kira_dynamic_call_status(call),
            KIRA_DYNAMIC_CALL_TOO_MANY_ARGS
        );
        assert_eq!(
            kira_dynamic_call_invoke_u64(call, add_one as *mut c_void),
            0
        );
        kira_dynamic_call_free(call);
    }
}

#[test]
fn a_null_target_is_refused_rather_than_jumped_to() {
    let call = call_of(1);
    // SAFETY: the call is live, and a null target never gets called.
    unsafe {
        kira_dynamic_call_arg_u64(call, 1);
        assert_eq!(
            kira_dynamic_call_invoke_i32(call, std::ptr::null_mut()),
            0,
            "a missing vtable slot must not become a jump to zero"
        );
        assert_eq!(
            kira_dynamic_call_status(call),
            KIRA_DYNAMIC_CALL_NULL_TARGET
        );
        kira_dynamic_call_free(call);
    }
}

#[test]
fn reset_clears_both_the_arguments_and_the_fault() {
    let call = call_of(1);
    // SAFETY: the call is live throughout.
    unsafe {
        kira_dynamic_call_arg_f64(call, 1.0);
        assert_ne!(kira_dynamic_call_status(call), KIRA_DYNAMIC_CALL_OK);
        kira_dynamic_call_reset(call);
        assert_eq!(kira_dynamic_call_status(call), KIRA_DYNAMIC_CALL_OK);
        kira_dynamic_call_arg_u64(call, 41);
        assert_eq!(
            kira_dynamic_call_invoke_u64(call, add_one as *mut c_void),
            42,
            "the builder is reusable, which is why reset exists"
        );
        kira_dynamic_call_free(call);
    }
}

#[test]
fn an_arity_above_the_ceiling_is_refused_at_creation() {
    let too_wide = KIRA_DYNAMIC_CALL_MAX_ARITY as u32 + 1;
    assert!(
        kira_dynamic_call_new(too_wide).is_null(),
        "refused where the caller can still see it, not after the arguments are built"
    );
}

#[test]
fn a_call_through_a_vtable_slot_reaches_the_method() {
    // The shape the D3D12 backend uses: an object whose first field is a
    // pointer to a table of function pointers, and a method read out by
    // slot index rather than by name.
    let vtable = crate::raw_memory::kira_dynamic_alloc(8 * 4);
    let object = crate::raw_memory::kira_dynamic_alloc(8);
    let call = call_of(1);
    // SAFETY: both blocks came from `kira_dynamic_alloc` and are large
    // enough for the offsets written; slot 2 holds `add_one`, whose
    // signature matches the single integer argument pushed.
    unsafe {
        crate::raw_memory::kira_dynamic_write_ptr_at(vtable, 8 * 2, add_one as *mut c_void);
        crate::raw_memory::kira_dynamic_write_ptr(object, vtable);

        let read_vtable = crate::raw_memory::kira_dynamic_read_ptr(object);
        let method = crate::raw_memory::kira_dynamic_read_ptr_at(read_vtable, 8 * 2);

        kira_dynamic_call_arg_u64(call, 41);
        assert_eq!(kira_dynamic_call_invoke_u64(call, method), 42);

        kira_dynamic_call_free(call);
        crate::raw_memory::kira_dynamic_free(object);
        crate::raw_memory::kira_dynamic_free(vtable);
    }
}

#[test]
fn freeing_null_does_nothing() {
    // SAFETY: null is the one pointer the contract explicitly allows.
    unsafe {
        kira_dynamic_call_free(std::ptr::null_mut());
        kira_dynamic_call_reset(std::ptr::null_mut());
    }
}
