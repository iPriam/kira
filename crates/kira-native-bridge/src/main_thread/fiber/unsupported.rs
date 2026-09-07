//! Fallback for targets without native lifecycle fibers.
//!
//! The compiler refuses `@MainThreadLifecycle` on these targets (`KSEM338` on
//! WebAssembly), so no start request can be reached at runtime; this module
//! keeps the bridge linking with the same scheduling surface.

/// Native body of one zero-argument lifecycle function.
pub(crate) type LifecycleEntry = extern "C" fn();

/// Refuses the start: this target owns no cooperative main-thread fibers.
pub(crate) fn start(_entry: LifecycleEntry) -> Result<(), &'static str> {
    Err("main-thread lifecycles are unsupported on this target")
}

/// Reports that no lifecycle work exists to advance.
pub(crate) fn pump(_budget: u64) -> bool {
    false
}

/// Reports that no lifecycle instance can be live on this target.
pub(crate) fn active() -> bool {
    false
}

/// Clears nothing: no lifecycle state exists on this target.
pub(crate) fn reset() {}
