//! The native half of Kira's environment reads: `kira_rt_env_*`.
//!
//! Each symbol here is the native mirror of one `Env` VM instruction, and both
//! go through the same [`kira_runtime_abi::env`] — the two engines share an
//! implementation rather than sharing a specification, which is what makes
//! byte-identical output something the code enforces instead of something a
//! test hopes for.
//!
//! # Ownership
//!
//! Affine, like the rest of this runtime: the name handle is freed before the
//! call returns, mirroring the VM dropping the operand it popped.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use crate::runtime::KStr;
use crate::values::{handle_of, release, text_of};

/// The value of an environment variable, empty when it is unset.
///
/// # Safety
/// `name` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_env_text(name: KStr) -> KStr {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(name) };
    // SAFETY: same.
    unsafe { release(name) };
    handle_of(&kira_runtime_abi::env::text(&text))
}

/// Whether an environment variable is set, however empty its value.
///
/// # Safety
/// `name` must be null or a live string handle, which this frees.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_env_is_set(name: KStr) -> u8 {
    // SAFETY: forwarded contract.
    let text = unsafe { text_of(name) };
    // SAFETY: same.
    unsafe { release(name) };
    u8::from(kira_runtime_abi::env::is_set(&text))
}

/// The number of user arguments passed to the process, excluding its path.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_env_argument_count() -> i64 {
    kira_runtime_abi::env::argument_count()
}

/// One user argument by zero-based index, or an empty string out of range.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_env_argument(index: i64) -> KStr {
    handle_of(&kira_runtime_abi::env::argument(index))
}

/// Pause the current process for a number of milliseconds.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_process_sleep(milliseconds: i64) {
    kira_runtime_abi::env::sleep(milliseconds)
}
