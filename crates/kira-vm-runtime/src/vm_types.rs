//! Shared VM data types: the C-ABI `NativeStateBox` token, the
//! exported-closure registry entry, and native-layout allocation statistics.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_types.zig`.

use crate::abi::Value;

/// VM-side native-state token.
///
/// Zig: `NativeStateBox` (`extern struct`). The leading three fields
/// (`type_id`, `payload`, `runtime_payload`) are the C-ABI prefix shared with
/// the native backend's `KiraNativeState` in
/// `packages/kira_native_bridge/src/runtime_helpers.c`. Everything after that
/// prefix is VM-internal metadata used to clean up VM-allocated payloads at
/// shutdown; the native backend never reads those fields.
///
/// Tokens are NOT cast across backends: VM tokens are always allocated and
/// read by the VM, and their `payload`/`runtime_payload` hold VM
/// `BridgeValue`/`Value` arrays, whereas the C path's payload is a raw byte
/// buffer with incompatible semantics. The Zig side enforces the shared
/// 3-field prefix with a comptime offset assertion; the Rust port asserts the
/// same offsets in a unit test once the layout carries real data.
#[repr(C)]
#[derive(Debug)]
pub struct NativeStateBox {
    /// Zig: `type_id: u64` — C prefix field 0 (offset 0).
    pub type_id: u64,
    /// Zig: `payload: usize` — C prefix field 1 (offset 8).
    pub payload: usize,
    /// Zig: `runtime_payload: usize` — C prefix field 2 (offset 16).
    pub runtime_payload: usize,
    /// Zig: `module: *const bytecode.Module` — VM-internal.
    /// TODO(port): becomes a real `kira_bytecode::Module` pointer/handle once
    /// kira-bytecode scaffolds its module type.
    pub module: *const core::ffi::c_void,
    /// Zig: `type_name_ptr: [*]const u8` — VM-internal, borrowed from module.
    pub type_name_ptr: *const u8,
    /// Zig: `type_name_len: usize` — VM-internal.
    pub type_name_len: usize,
    /// Zig: `field_count: usize` — VM-internal.
    pub field_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the Zig comptime assertion: the 3-field `KiraNativeState` C
    /// prefix must match exactly.
    #[test]
    fn native_state_box_keeps_the_c_prefix_layout() {
        assert_eq!(core::mem::offset_of!(NativeStateBox, type_id), 0);
        assert_eq!(
            core::mem::offset_of!(NativeStateBox, payload),
            size_of::<u64>()
        );
        assert_eq!(
            core::mem::offset_of!(NativeStateBox, runtime_payload),
            size_of::<u64>() + size_of::<usize>()
        );
    }
}

/// A VM closure exported to native code. Zig: `ExportedNativeClosure`.
#[derive(Debug)]
pub struct ExportedNativeClosure {
    /// Zig: `native_ptr: usize` — the native-visible closure handle.
    pub native_ptr: usize,
    /// Zig: `captures: []runtime_abi.Value` — owned captured slots.
    pub captures: Box<[Value]>,
}

/// Native-layout allocation statistics. Zig: `NativeLayoutStats`
/// (all `usize`, default 0).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NativeLayoutStats {
    pub arrays_current: usize,
    pub arrays_peak: usize,
    pub arrays_allocated: usize,
    pub arrays_freed: usize,
    pub structs_current: usize,
    pub structs_peak: usize,
    pub structs_allocated: usize,
    pub structs_freed: usize,
    pub native_state_recovers: usize,
    pub native_state_materializations: usize,
}
