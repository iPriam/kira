//! Calling a function whose signature is known only at run time.
//!
//! A declarative `@FFI.Extern` binding covers a C API whose entry points are
//! exported symbols: the compiler sees the signature, and the backend emits a
//! direct call. COM does not work that way. An `ID3D12Device` is a pointer to a
//! pointer to a table of function pointers, and the method being called is a
//! slot index in that table — a number the compiler never sees. There is no
//! symbol to bind, so there is nothing for `@FFI.Extern` to name.
//!
//! This is how such a call is made instead. The caller reads the slot out of
//! the vtable itself (with the pointer readers in [`crate::raw_memory`]),
//! builds an argument list here, and invokes the address:
//!
//! ```text
//! vtable = read_ptr(object)              // object->lpVtbl
//! method = read_ptr_at(vtable, slot * 8) // lpVtbl->CreateCommandQueue
//! call   = call_new(3)
//! call_arg_ptr(call, object)             // `this` is always argument zero
//! call_arg_ptr(call, desc)
//! call_arg_ptr(call, out)
//! hr     = call_invoke_i32(call, method) // HRESULT
//! ```
//!
//! # How the call is actually made
//!
//! By transmuting the target to a function pointer of the recorded arity and
//! calling it. Every argument is widened to a `usize` and passed in an integer
//! register or stack slot, which is where a C caller would have put an integer,
//! a pointer, or anything narrower — the register a value arrives in depends on
//! its class and position, not on the width the callee declares.
//!
//! # Why a float argument is refused rather than passed
//!
//! Because that equivalence stops at the integer classes. On the Microsoft x64
//! convention a `float` in argument position 2 arrives in `XMM2`, and on
//! SysV it consumes an SSE register rather than an integer one; widening it to
//! a `usize` here would deliver it in `R8` instead, and the callee would read
//! whatever `XMM2` happened to hold. That is a wrong answer with no crash to
//! find it by, so [`kira_dynamic_call_arg_f32`] and [`kira_dynamic_call_arg_f64`]
//! record [`KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED`] and every later invoke on
//! that call refuses.
//!
//! Nothing in D3D12's COM surface or Vulkan's entry points needs one: both pass
//! floats inside structures, behind a pointer. Lifting the limit means either a
//! libffi dependency or one transmute per int/float position pattern, and
//! should wait for an API that genuinely requires it.
//!
//! A float *return* is fine and supported — it comes back in `XMM0`/`v0`
//! whatever the arguments were, and [`kira_dynamic_call_invoke_f32`] transmutes
//! to a function type returning one.

use std::ffi::c_void;

/// The call is well formed; its last invoke ran.
pub const KIRA_DYNAMIC_CALL_OK: i32 = 0;

/// A float argument was pushed, which this builder cannot pass correctly.
pub const KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED: i32 = 1;

/// More arguments were pushed than the builder was created to hold.
pub const KIRA_DYNAMIC_CALL_TOO_MANY_ARGS: i32 = 2;

/// The argument count is above [`KIRA_DYNAMIC_CALL_MAX_ARITY`].
pub const KIRA_DYNAMIC_CALL_ARITY_UNSUPPORTED: i32 = 3;

/// An invoke was handed a null function pointer.
pub const KIRA_DYNAMIC_CALL_NULL_TARGET: i32 = 4;

/// The most arguments one call may carry.
///
/// Sixteen clears the widest signature either target API has: D3D12's is
/// `ID3D12Device10::CreateCommittedResource3` at eleven including `this`, and
/// Vulkan's is `vkCmdPipelineBarrier` at eleven.
pub const KIRA_DYNAMIC_CALL_MAX_ARITY: usize = 16;

/// An argument list being assembled for a call to a run-time address.
///
/// Handed out as an opaque `*mut c_void`: a caller in Kira holds it as a
/// `RawPtr` and never sees the fields.
struct DynamicCall {
    /// Arguments pushed so far, each widened to an integer register's width.
    args: Vec<usize>,
    /// The count [`kira_dynamic_call_new`] was asked for.
    capacity: usize,
    /// The first fault seen, sticky until [`kira_dynamic_call_reset`].
    ///
    /// Sticky on purpose: a caller assembling eight arguments should not have
    /// to check after each one, and a call that lost an argument must not run.
    status: i32,
}

mod arity;

#[cfg(test)]
mod tests;

use arity::invoke_with_args;

/// Creates a call that can hold `max_args` arguments, or null when it cannot.
///
/// `max_args` is the caller's own bound, checked so a lost argument is a
/// reported fault rather than a call made with the wrong shape. An arity above
/// [`KIRA_DYNAMIC_CALL_MAX_ARITY`] is refused here rather than at invoke, where
/// the arguments would already have been assembled.
#[unsafe(no_mangle)]
pub extern "C" fn kira_dynamic_call_new(max_args: u32) -> *mut c_void {
    let capacity = max_args as usize;
    if capacity > KIRA_DYNAMIC_CALL_MAX_ARITY {
        return std::ptr::null_mut();
    }
    let call = Box::new(DynamicCall {
        args: Vec::with_capacity(capacity),
        capacity,
        status: KIRA_DYNAMIC_CALL_OK,
    });
    Box::into_raw(call).cast::<c_void>()
}

/// Empties `call` so it can assemble another call, clearing its status.
///
/// Keeps the allocation, which is the point: a frame that issues the same
/// command list every time reuses one builder instead of allocating per call.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_reset(call: *mut c_void) {
    if call.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`
    // and is still live, so it addresses an initialized `DynamicCall`.
    let call = unsafe { &mut *call.cast::<DynamicCall>() };
    call.args.clear();
    call.status = KIRA_DYNAMIC_CALL_OK;
}

/// Frees a call from [`kira_dynamic_call_new`].
///
/// A null pointer frees nothing, which is what C callers expect of `free`.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not already been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_free(call: *mut c_void) {
    if call.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`
    // and has not been freed, so reclaiming the `Box` is sound and happens once.
    drop(unsafe { Box::from_raw(call.cast::<DynamicCall>()) });
}

/// The first fault recorded on `call`, or [`KIRA_DYNAMIC_CALL_OK`].
///
/// Returns [`KIRA_DYNAMIC_CALL_NULL_TARGET`] for a null `call`, because a
/// caller that lost its builder has nothing to invoke either.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_status(call: *const c_void) -> i32 {
    if call.is_null() {
        return KIRA_DYNAMIC_CALL_NULL_TARGET;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`
    // and is still live.
    unsafe { (*call.cast::<DynamicCall>()).status }
}

/// Pushes one integer-class argument, recording an overflow rather than growing.
///
/// # Safety
/// `call` must be null, or a live pointer from [`kira_dynamic_call_new`].
unsafe fn push_arg(call: *mut c_void, value: usize) {
    if call.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`
    // and is still live.
    let call = unsafe { &mut *call.cast::<DynamicCall>() };
    if call.args.len() >= call.capacity {
        call.status = KIRA_DYNAMIC_CALL_TOO_MANY_ARGS;
        return;
    }
    call.args.push(value);
}

/// Records that a float argument was pushed, which cannot be passed correctly.
///
/// # Safety
/// `call` must be null, or a live pointer from [`kira_dynamic_call_new`].
unsafe fn refuse_float_arg(call: *mut c_void) {
    if call.is_null() {
        return;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`
    // and is still live.
    let call = unsafe { &mut *call.cast::<DynamicCall>() };
    if call.status == KIRA_DYNAMIC_CALL_OK {
        call.status = KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED;
    }
}

/// Defines one argument pusher that widens an integer-class value.
macro_rules! integer_arg {
    ($name:ident, $ty:ty, $doc:literal) => {
        #[doc = $doc]
        ///
        /// # Safety
        /// `call` must be null, or a pointer from [`kira_dynamic_call_new`]
        /// that has not been freed.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(call: *mut c_void, value: $ty) {
            // SAFETY: forwarded caller contract — `call` is null or live.
            unsafe { push_arg(call, value as usize) };
        }
    };
}

integer_arg!(
    kira_dynamic_call_arg_i32,
    i32,
    "Pushes a signed 32-bit argument, sign-extended to a register."
);
integer_arg!(
    kira_dynamic_call_arg_u32,
    u32,
    "Pushes an unsigned 32-bit argument, zero-extended to a register."
);
integer_arg!(
    kira_dynamic_call_arg_i64,
    i64,
    "Pushes a signed 64-bit argument."
);
integer_arg!(
    kira_dynamic_call_arg_u64,
    u64,
    "Pushes an unsigned 64-bit argument."
);

/// Pushes a pointer argument — a `this`, a descriptor, or an out-parameter.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_arg_ptr(call: *mut c_void, value: *mut c_void) {
    // SAFETY: forwarded caller contract — `call` is null or live.
    unsafe { push_arg(call, value as usize) };
}

/// Refuses a 32-bit float argument; see this module's header for why.
///
/// Records [`KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED`], and every later invoke
/// on `call` returns zero without calling anything.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_arg_f32(call: *mut c_void, _value: f32) {
    // SAFETY: forwarded caller contract — `call` is null or live.
    unsafe { refuse_float_arg(call) };
}

/// Refuses a 64-bit float argument; see this module's header for why.
///
/// Records [`KIRA_DYNAMIC_CALL_FLOAT_ARG_UNSUPPORTED`], and every later invoke
/// on `call` returns zero without calling anything.
///
/// # Safety
/// `call` must be null, or a pointer from [`kira_dynamic_call_new`] that has
/// not been freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_arg_f64(call: *mut c_void, _value: f64) {
    // SAFETY: forwarded caller contract — `call` is null or live.
    unsafe { refuse_float_arg(call) };
}

/// Validates a call and its target, yielding the arguments to invoke with.
///
/// Returns `None` — and leaves a status behind — for every reason the call must
/// not be made, so each invoke below has one refusal path and no judgement.
///
/// # Safety
/// `call` must be null, or a live pointer from [`kira_dynamic_call_new`], and
/// must not be aliased for the lifetime of the returned borrow.
unsafe fn prepare<'a>(call: *mut c_void, target: *mut c_void) -> Option<&'a mut DynamicCall> {
    if call.is_null() {
        return None;
    }
    // SAFETY: the caller guarantees `call` came from `kira_dynamic_call_new`,
    // is still live, and is not aliased while this borrow is held.
    let call = unsafe { &mut *call.cast::<DynamicCall>() };
    if call.status != KIRA_DYNAMIC_CALL_OK {
        return None;
    }
    if target.is_null() {
        call.status = KIRA_DYNAMIC_CALL_NULL_TARGET;
        return None;
    }
    if call.args.len() > KIRA_DYNAMIC_CALL_MAX_ARITY {
        call.status = KIRA_DYNAMIC_CALL_ARITY_UNSUPPORTED;
        return None;
    }
    Some(call)
}

/// Defines one invoke that returns a value.
macro_rules! invoke {
    ($name:ident, $ty:ty, $zero:expr, $doc:literal) => {
        #[doc = $doc]
        ///
        /// Returns zero without calling anything when `call` carries a fault —
        /// read [`kira_dynamic_call_status`] to tell that from a genuine zero.
        ///
        /// # Safety
        /// `call` must be a live pointer from [`kira_dynamic_call_new`], and
        /// `function_ptr` must address a function taking exactly the arguments
        /// pushed onto `call`, each integer- or pointer-class, returning this
        /// type.
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(call: *mut c_void, function_ptr: *mut c_void) -> $ty {
            // SAFETY: forwarded caller contract — `call` is live and unaliased.
            let Some(call) = (unsafe { prepare(call, function_ptr) }) else {
                return $zero;
            };
            invoke_with_args!(function_ptr, &call.args, $ty, $zero)
        }
    };
}

invoke!(
    kira_dynamic_call_invoke_i32,
    i32,
    0,
    "Invokes `function_ptr`, reading its result as a signed 32-bit value — an `HRESULT` or a `VkResult`."
);
invoke!(
    kira_dynamic_call_invoke_u32,
    u32,
    0,
    "Invokes `function_ptr`, reading its result as an unsigned 32-bit value — a `ULONG` reference count."
);
invoke!(
    kira_dynamic_call_invoke_i64,
    i64,
    0,
    "Invokes `function_ptr`, reading its result as a signed 64-bit value."
);
invoke!(
    kira_dynamic_call_invoke_u64,
    u64,
    0,
    "Invokes `function_ptr`, reading its result as an unsigned 64-bit value — a GPU virtual address, or a descriptor handle returned whole in a register."
);
invoke!(
    kira_dynamic_call_invoke_f32,
    f32,
    0.0,
    "Invokes `function_ptr`, reading its result as a 32-bit float."
);
invoke!(
    kira_dynamic_call_invoke_f64,
    f64,
    0.0,
    "Invokes `function_ptr`, reading its result as a 64-bit float."
);

/// Invokes `function_ptr`, reading its result as a pointer.
///
/// Returns null without calling anything when `call` carries a fault — read
/// [`kira_dynamic_call_status`] to tell that from a genuine null.
///
/// # Safety
/// `call` must be a live pointer from [`kira_dynamic_call_new`], and
/// `function_ptr` must address a function taking exactly the arguments pushed
/// onto `call`, each integer- or pointer-class, returning a pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_invoke_ptr(
    call: *mut c_void,
    function_ptr: *mut c_void,
) -> *mut c_void {
    // SAFETY: forwarded caller contract — `call` is live and unaliased.
    let Some(call) = (unsafe { prepare(call, function_ptr) }) else {
        return std::ptr::null_mut();
    };
    invoke_with_args!(
        function_ptr,
        &call.args,
        *mut c_void,
        std::ptr::null_mut::<c_void>()
    )
}

/// Invokes `function_ptr` for its effect, discarding any result.
///
/// # Safety
/// `call` must be a live pointer from [`kira_dynamic_call_new`], and
/// `function_ptr` must address a function taking exactly the arguments pushed
/// onto `call`, each integer- or pointer-class.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_dynamic_call_invoke_void(
    call: *mut c_void,
    function_ptr: *mut c_void,
) {
    // SAFETY: forwarded caller contract — `call` is live and unaliased.
    let Some(call) = (unsafe { prepare(call, function_ptr) }) else {
        return;
    };
    invoke_with_args!(function_ptr, &call.args, (), ())
}
