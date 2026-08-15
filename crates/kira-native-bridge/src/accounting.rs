//! Counting what the native runtime allocates and frees, so a native run can
//! prove its heap balanced.
//!
//! The VM proves this with its heap counters, and the native runtime exposes the
//! same proof through [`kira_rt_heap_live`]. Every helper that really allocates
//! records one, every helper that really frees records one, and a balanced run
//! reports equal allocation and free totals with no live objects.
//!
//! # Real allocations only
//!
//! A share bump is not an allocation and a share release is not a free: an
//! array or enum with two holders was allocated once and is reclaimed once. So
//! the counters move where the memory does — after the pool hands a block out,
//! and on the branch of a free that actually reclaims it — never on the paths
//! that only adjust a count. An inline enum handle is never counted at all,
//! because nothing was ever allocated for it.
//!
//! # Why it is compiled out of a release build
//!
//! The pool exists because `malloc`/`free` were 36% of a Project Matter frame,
//! and an unconditional atomic on every enum construction would give part of
//! that back for something only a test reads. So the counting is behind
//! `debug_assertions`: on for the dev and test builds where the proof is
//! wanted, absent from a release build entirely.
//!
//! That makes "zero live" ambiguous on its own — a release build reports zero
//! because it counted nothing. [`kira_rt_heap_accounting_enabled`] is how a
//! caller tells the two apart, and a caller that does not ask has no business
//! concluding anything from the count.

#[cfg(debug_assertions)]
use std::sync::atomic::{AtomicU64, Ordering};

/// Blocks handed out, since the process started.
#[cfg(debug_assertions)]
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// Blocks reclaimed, since the process started.
#[cfg(debug_assertions)]
static FREED: AtomicU64 = AtomicU64::new(0);

/// Records one real allocation.
///
/// Relaxed ordering: these are counters nobody synchronizes on, read once at
/// the end of a run, and the runtime's own premise is that a Kira value is
/// touched by one thread at a time — the share counts in [`crate::enums`] rest
/// on the same thing and would corrupt before a counter did.
#[inline]
pub fn record_alloc() {
    #[cfg(debug_assertions)]
    ALLOCATED.fetch_add(1, Ordering::Relaxed);
}

/// Records one real free.
#[inline]
pub fn record_free() {
    #[cfg(debug_assertions)]
    FREED.fetch_add(1, Ordering::Relaxed);
}

/// Blocks the native runtime has allocated, or zero when not counting.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_heap_allocated() -> u64 {
    #[cfg(debug_assertions)]
    {
        ALLOCATED.load(Ordering::Relaxed)
    }
    #[cfg(not(debug_assertions))]
    {
        0
    }
}

/// Blocks the native runtime has freed, or zero when not counting.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_heap_freed() -> u64 {
    #[cfg(debug_assertions)]
    {
        FREED.load(Ordering::Relaxed)
    }
    #[cfg(not(debug_assertions))]
    {
        0
    }
}

/// Blocks allocated and not yet freed: zero for a run that balanced.
///
/// Saturating rather than wrapping, so a double free reports zero live rather
/// than about eighteen quintillion — the count is evidence, and evidence that
/// reads as a wild number teaches a reader nothing about what went wrong.
/// A double free is caught by the free itself, not by this.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_heap_live() -> u64 {
    kira_rt_heap_allocated().saturating_sub(kira_rt_heap_freed())
}

/// Whether this build counts at all.
///
/// The one thing that separates "balanced" from "never measured": both report
/// zero live, and only this says which.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_heap_accounting_enabled() -> bool {
    cfg!(debug_assertions)
}

/// The variable that asks a native run to report its heap balance at exit.
pub const HEAP_REPORT_VAR: &str = "KIRA_HEAP_REPORT";

/// Reports the heap balance at the end of a native run, when asked to.
///
/// The emitted `main` calls this immediately before returning, so it is the
/// native counterpart of the VM's `current == 0` check — the difference being
/// that the VM's heap is a Rust value a caller can inspect, and a native
/// program's is gone the moment the process is.
///
/// Silent unless [`HEAP_REPORT_VAR`] is set: an ordinary run pays one `getenv`
/// at exit and prints nothing, and a test that wants the proof asks for it.
///
/// An unbalanced run exits non-zero *after* the program's own output, so a test
/// sees both what the program printed and that it leaked. A build that was not
/// counting says so rather than reporting a balance it never measured.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_heap_report() {
    if std::env::var_os(HEAP_REPORT_VAR).is_none() {
        return;
    }
    if !kira_rt_heap_accounting_enabled() {
        eprintln!("kira: heap accounting is not compiled into this build");
        return;
    }
    let (allocated, freed, live) = (
        kira_rt_heap_allocated(),
        kira_rt_heap_freed(),
        kira_rt_heap_live(),
    );
    eprintln!("kira: heap allocated={allocated} freed={freed} live={live}");
    if live != 0 {
        eprintln!("kira: the native heap did not balance: {live} object(s) leaked");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An allocation and its free cancel, which is the whole contract.
    ///
    /// Read as a delta rather than as an absolute: these counters are
    /// process-wide and every other test in this binary moves them, so an
    /// assertion on the totals would depend on test order.
    #[test]
    fn a_matched_pair_leaves_the_live_count_where_it_was() {
        let before = kira_rt_heap_live();
        record_alloc();
        assert_eq!(kira_rt_heap_live(), before + 1);
        record_free();
        assert_eq!(kira_rt_heap_live(), before);
    }

    /// The counters are live in a test build, which is where the proof is used.
    ///
    /// A build that silently stopped counting would make every balance
    /// assertion pass by measuring nothing — the exact failure this flag
    /// exists to make visible.
    #[test]
    fn a_test_build_is_counting() {
        assert!(
            kira_rt_heap_accounting_enabled(),
            "tests run with debug assertions, so the counters must be live"
        );
        let before = kira_rt_heap_allocated();
        record_alloc();
        assert!(kira_rt_heap_allocated() > before);
        record_free();
    }
}
