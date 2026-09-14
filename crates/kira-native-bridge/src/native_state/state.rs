use kira_runtime_abi::{NativeStateStatus, NativeStateToken, NativeStateTypeId};

use super::{KNativeStateValue, boxed, decode_path, finish, status, store};

/// Boxes a completed value node and writes its stable token to `out`.
///
/// # Safety
/// `value` must be one live node the call consumes, and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_new(
    type_id: u64,
    value: KNativeStateValue,
    out: *mut u64,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.create(NativeStateTypeId::new(type_id), value) {
        Ok(token) => {
            // SAFETY: the caller supplies one writable word.
            unsafe { *out = token.as_word() };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Boxes a completed value node that carries a user `Drop` body and writes its
/// stable token to `out`.
///
/// The store cannot execute Kira code itself, so it records `glue`; the final
/// destructive release hands both the glue id and value tree back to generated
/// code, which is the engine capable of entering the body.
///
/// # Safety
/// `value` must be one live node the call consumes, and `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_new_dropping(
    type_id: u64,
    value: KNativeStateValue,
    glue: u32,
    out: *mut u64,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.create_dropping(NativeStateTypeId::new(type_id), value, Some(glue)) {
        Ok(token) => {
            // SAFETY: the caller supplies one writable word.
            unsafe { *out = token.as_word() };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Recovers a typed owned copy into `out`.
///
/// # Safety
/// `out` must be writable when non-null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_recover(
    token: u64,
    type_id: u64,
    out: *mut KNativeStateValue,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.recover(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
    ) {
        Ok(value) => {
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out = boxed(value) };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Replaces typed state with a completed value node and returns the displaced root.
///
/// # Safety
/// `value` must be one live node the call consumes, and `out_old` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_replace(
    token: u64,
    type_id: u64,
    value: KNativeStateValue,
    out_old: *mut KNativeStateValue,
) -> u32 {
    if out_old.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.replace(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
        value,
    ) {
        Ok(old) => {
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out_old = boxed(old) };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Reads one value addressed inside callback state into a generic value node.
///
/// # Safety
/// `out` is writable. When `count` is non-zero, `kinds` and `values` are
/// readable for `count` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_read_path(
    token: u64,
    type_id: u64,
    kinds: *const u8,
    values: *const u64,
    count: usize,
    out: *mut KNativeStateValue,
) -> u32 {
    if out.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: upheld by this function's caller contract.
    let path = match unsafe { decode_path(kinds, values, count) } {
        Ok(path) => path,
        Err(status) => return status.0,
    };
    let store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.read_at(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
        &path,
    ) {
        Ok(value) => {
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out = boxed(value.clone()) };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Replaces one value addressed inside callback state and returns exactly the
/// displaced owner. The path is carried as parallel tag/value arrays so dynamic
/// array indices cross without allocating a temporary aggregate node.
///
/// # Safety
/// `value` is one live node consumed by this call. `out_old` is writable.
/// When `count` is non-zero, `kinds` and `values` are readable for `count`
/// entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_write_path(
    token: u64,
    type_id: u64,
    kinds: *const u8,
    values: *const u64,
    count: usize,
    value: KNativeStateValue,
    out_old: *mut KNativeStateValue,
) -> u32 {
    if out_old.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: upheld by this function's caller contract.
    let path = match unsafe { decode_path(kinds, values, count) } {
        Ok(path) => path,
        Err(status) => return status.0,
    };
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.replace_at(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
        &path,
        value,
    ) {
        Ok(old) => {
            // SAFETY: the caller supplies one writable pointer slot.
            unsafe { *out_old = boxed(old) };
            NativeStateStatus::OK.0
        }
        Err(error) => status(error),
    }
}

/// Appends one value to the array addressed inside callback state.
///
/// # Safety
/// `value` is one live node consumed by this call. When `count` is non-zero,
/// `kinds` and `values` are readable for `count` entries.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_append_path(
    token: u64,
    type_id: u64,
    kinds: *const u8,
    values: *const u64,
    count: usize,
    value: KNativeStateValue,
) -> u32 {
    // SAFETY: upheld by this function's caller contract.
    let path = match unsafe { decode_path(kinds, values, count) } {
        Ok(path) => path,
        Err(status) => return status.0,
    };
    let value = match finish(value) {
        Ok(value) => value,
        Err(status) => return status.0,
    };
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.append_at(
        NativeStateToken::from_word(token),
        NativeStateTypeId::new(type_id),
        &path,
        value,
    ) {
        Ok(()) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Adds one owner to a state token.
///
/// One path for both kinds of state, told apart by the token itself: a native
/// engine's state is a box it owns, and the value-tree store never hands out an
/// odd token. See `crate::state_box`.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_state_retain(token: u64) -> u32 {
    if crate::state_box::is_box_token(token) {
        // SAFETY: the token is a box token this runtime handed out.
        return unsafe { crate::state_box::kira_rt_native_state_box_retain(token) };
    }
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.retain(NativeStateToken::from_word(token)) {
        Ok(()) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Removes one owner from a state token; the last release destroys the state.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_state_release(token: u64) -> u32 {
    if crate::state_box::is_box_token(token) {
        // SAFETY: the token is a box token this runtime handed out, and every
        // owner releases it once.
        return unsafe { crate::state_box::kira_rt_native_state_box_free(token) };
    }
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.release(NativeStateToken::from_word(token)) {
        Ok(_) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Removes one owner and returns the owned tree when final destruction needs an
/// executing Kira engine. The tree itself carries all nested user `Drop` glue.
/// `out_value` is null for a non-destroying release and for boxed native state,
/// whose generated free leaf executes directly.
///
/// # Safety
/// `out_value` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_native_state_release_dropping(
    token: u64,
    out_value: *mut KNativeStateValue,
) -> u32 {
    if out_value.is_null() {
        return NativeStateStatus::MALFORMED_VALUE.0;
    }
    // SAFETY: the output was checked non-null above.
    unsafe { *out_value = std::ptr::null_mut() };
    if crate::state_box::is_box_token(token) {
        // SAFETY: the token is a box token this runtime handed out.
        return unsafe { crate::state_box::kira_rt_native_state_box_free(token) };
    }
    let mut store = match store().lock() {
        Ok(store) => store,
        Err(poisoned) => poisoned.into_inner(),
    };
    match store.release_dropping(NativeStateToken::from_word(token)) {
        Ok(Some(value)) => {
            // SAFETY: the output is writable for this call.
            unsafe { *out_value = boxed(value) };
            NativeStateStatus::OK.0
        }
        Ok(None) => NativeStateStatus::OK.0,
        Err(error) => status(error),
    }
}

/// Releases one owner of a state token: the name native code compiled against
/// before releases were counted, kept so that code keeps linking.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_native_state_free(token: u64) -> u32 {
    kira_rt_native_state_release(token)
}

/// Terminates native execution with a deterministic callback-state trap.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_trap_native_state(status: u32) -> ! {
    let message = match NativeStateStatus(status) {
        NativeStateStatus::NO_HOST => "no callback-state host",
        NativeStateStatus::NULL_TOKEN => "null callback-state token",
        NativeStateStatus::UNKNOWN_TOKEN => "unknown or already-freed callback-state token",
        NativeStateStatus::WRONG_TYPE => "callback-state type mismatch",
        NativeStateStatus::TOKEN_EXHAUSTED => "callback-state token space exhausted",
        NativeStateStatus::MALFORMED_VALUE => "malformed callback-state value",
        NativeStateStatus::DROP_ENGINE_REQUIRED => {
            "callback-state destruction requires an executing Kira engine"
        }
        _ => "unknown callback-state failure",
    };
    eprintln!("kira: runtime trap: {message}");
    std::process::exit(1)
}
