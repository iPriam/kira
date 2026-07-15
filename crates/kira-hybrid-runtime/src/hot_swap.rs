//! Live hot swap: replace the executing bytecode module (and its hybrid
//! manifest) inside a RUNNING `HybridRuntime` without tearing down the
//! process, the window, sokol, the native dylib, the VM heap, or any app
//! state.
//!
//! Model (ported note from Zig): a background listener STAGES a swap; the
//! main thread APPLIES it at the next native->runtime callback boundary,
//! when no VM code is on the stack and the task queue is idle; the old
//! module/manifest are RETIRED, not freed — live heap values borrow
//! type-name and string-literal slices from module memory for as long as
//! they live. A swap is REJECTED (fall back to relaunch) when the edit
//! changed something live values depend on: a struct/enum layout, or the
//! signature of a function a live closure references.
//!
//! Ported from kira-zig `packages/kira_hybrid_runtime/src/hot_swap.zig`
//! (compatibility checks in `hot_swap_compat.zig`).

/// A fully-loaded replacement program, staged by the reload listener thread
/// and consumed by the main thread at a safe boundary. Zig: `StagedSwap`
/// (`module: bytecode.Module`, `manifest: hybrid.HybridModuleManifest`).
/// TODO(port): fields land once kira-bytecode / kira-hybrid-definition
/// scaffold their types.
#[derive(Debug, Default)]
pub struct StagedSwap {}

/// Reload lifecycle events reported to the live runner. Zig: `ReloadEvent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReloadEvent {
    /// The swap was committed; the NEXT dispatch already runs new code.
    Applied,
    /// The first callback after the swap finished — real new-code execution.
    Completed,
    /// The swap cannot be applied in place; caller should fall back to a
    /// process relaunch.
    Rejected,
}

/// Reload state carried by a `HybridRuntime`: the staged swap (if any) and
/// the retired module list. Zig: `ReloadState` in `hot_swap.zig`.
#[derive(Debug, Default)]
pub struct ReloadState {
    /// Swap staged by the listener thread, awaiting a safe boundary.
    pub staged: Option<StagedSwap>,
    /// Retired programs kept alive for borrowed type-name/string bytes.
    pub retired: Vec<StagedSwap>,
}
