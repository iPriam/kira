//! Glue for the LLVM C API: building the strings it takes and reclaiming the
//! ones it hands back.
//!
//! Every LLVM-C entry point speaks NUL-terminated `char *`, and the ones that
//! report failure allocate their message with LLVM's own allocator. Both halves
//! of that live here so no caller has to remember which strings it owns.

use std::ffi::{CStr, CString};

use llvm_sys::core::LLVMDisposeMessage;

/// Builds a NUL-terminated copy of `text` for the C API.
///
/// Interior NUL bytes are replaced rather than rejected: these strings are
/// module and symbol names, where a NUL cannot carry meaning, and a name is
/// never a reason to fail a build.
pub(super) fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "_")).unwrap_or_else(|_| CString::default())
}

/// Takes ownership of an LLVM-allocated message, returning it as a `String`.
///
/// # Safety
/// `message` must be null or a NUL-terminated string LLVM allocated.
pub(super) unsafe fn take_message(message: *mut std::os::raw::c_char) -> String {
    if message.is_null() {
        return "LLVM reported no detail".to_owned();
    }
    // SAFETY: the caller guarantees a NUL-terminated LLVM-allocated string;
    // the text is copied out before the allocation is released.
    unsafe {
        let text = CStr::from_ptr(message).to_string_lossy().into_owned();
        LLVMDisposeMessage(message);
        text
    }
}

/// Takes ownership of an [`LLVMErrorRef`](llvm_sys::error::LLVMErrorRef)'s
/// message, returning it as a `String` and consuming the error itself.
///
/// An error's message is freed by `LLVMDisposeErrorMessage`, not by
/// `LLVMDisposeMessage`, and the error ref by `LLVMConsumeError` — a pairing
/// that must not drift onto the generic message helpers even where both
/// allocators happen to be plain `free` today.
///
/// # Safety
/// `error` must be a live `LLVMErrorRef`, consumed exactly once.
pub(super) unsafe fn take_error(error: llvm_sys::error::LLVMErrorRef) -> String {
    use llvm_sys::error::{LLVMConsumeError, LLVMDisposeErrorMessage, LLVMGetErrorMessage};
    // SAFETY: the caller guarantees a live error ref; the message is copied
    // out with its own disposer, then the error itself is consumed.
    unsafe {
        let message = LLVMGetErrorMessage(error);
        let mut text = "LLVM reported no detail".to_owned();
        if !message.is_null() {
            text = CStr::from_ptr(message).to_string_lossy().into_owned();
        }
        LLVMDisposeErrorMessage(message);
        LLVMConsumeError(error);
        text
    }
}

/// Releases an LLVM-allocated message, if any.
///
/// # Safety
/// `message` must be null or a string LLVM allocated.
pub(super) unsafe fn dispose_message(message: *mut std::os::raw::c_char) {
    if !message.is_null() {
        // SAFETY: the caller guarantees an LLVM-allocated string.
        unsafe { LLVMDisposeMessage(message) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_interior_nuls_still_build_a_c_string() {
        assert_eq!(c_string("a\0b").to_bytes(), b"a_b");
    }
}
