//! Native-layout marshaling: converts VM heap values to/from the C-ABI
//! layouts native code expects (arrays, structs, strings, enums), tracking
//! materializations in `NativeLayoutStats`.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/native_layout.zig`.
//! Logic lands with the bridge port.
