//! The `Vm` container: owns the heap, module registry, native-state
//! tracking, task runtime, hooks, and debug controller. Behavior lives in the
//! focused `vm_*` modules; this file stays the state container plus its
//! lifecycle, mirroring the Zig split.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm.zig`.

use crate::ownership::Heap;
use crate::vm_types::NativeLayoutStats;

/// The virtual machine instance.
///
/// Zig: `Vm` in `vm.zig` (allocator, heap, prepared modules, native-state
/// tracking, exported closures, task queue, hooks, debug state). Scaffold:
/// only the fields with self-contained types are present; the rest land with
/// their owning modules' ports.
#[derive(Debug, Default)]
pub struct Vm {
    /// Managed-object heap. Zig: `heap: ownership.Heap`.
    pub heap: Heap,
    /// Native-layout materialization statistics.
    /// Zig: `native_layout_stats: NativeLayoutStats`.
    pub native_layout_stats: NativeLayoutStats,
    // TODO(port): prepared-module table (vm_prepare), tracked native states
    // (vm_types::NativeStateBox), exported closures, hooks (runtime invoke /
    // print / first-frame), task runtime state (vm_tasks), and the debug
    // controller (vm_debug) as those modules land.
}
