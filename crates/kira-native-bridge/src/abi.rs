//! C-ABI bridge types matching the structs declared in kira-zig
//! `packages/kira_native_bridge/src/runtime_helpers.c`.
//!
//! The value family (`KiraBridgeValue` & co.) is the same layout as the
//! runtime-ABI types, so those are aliases into `kira-runtime-abi` — exactly
//! one definition in the workspace (layout tests live there). The C names are
//! kept as this crate's surface because this crate mirrors the C header side.
//! `KiraArray` and `KiraNativeState` are native-bridge-specific and stay here.

use core::ffi::c_void;

/// C: `KiraBridgeString` (`{ const unsigned char *ptr; size_t len; }`).
pub type KiraBridgeString = kira_runtime_abi::BridgeString;

/// C: `KiraBridgePayload` (union; the C `float64` field is Rust `float`).
pub type KiraBridgePayload = kira_runtime_abi::BridgePayload;

/// C: `KiraBridgeValue` (`{ uint8_t tag; uint8_t reserved[7]; payload; }`).
/// Tag values follow `KiraBridgeValueTag`: 0 void, 1 integer, 2 float,
/// 3 string, 4 boolean, 5 raw_ptr; unknown tags degrade to void.
pub type KiraBridgeValue = kira_runtime_abi::BridgeValue;

/// C: `KiraArray` (`{ size_t len; KiraBridgeValue *items; size_t cap; }`).
///
/// Capacity invariant shared with the VM heap
/// (`kira_vm_runtime::ownership::ArrayObject`): the `items` buffer is always
/// exactly `max(cap, 1)` elements, `len <= cap`. Appends grow geometrically
/// through `cap`, replacing the old grow-by-one realloc that made building an
/// n-element array O(n^2) memcpy.
#[repr(C)]
#[derive(Debug)]
pub struct KiraArray {
    pub len: usize,
    pub items: *mut KiraBridgeValue,
    pub cap: usize,
}

/// C: `KiraNativeState` — native-backend native-state token.
///
/// These three fields are the C-ABI prefix shared with the VM's
/// `kira_vm_runtime::vm_types::NativeStateBox`. The VM appends VM-internal
/// metadata after this prefix, but tokens are never cast across backends: the
/// native path allocates/reads only this 3-field struct, and its `payload` is
/// a raw byte buffer, while the VM's payload holds VM value arrays. Keep the
/// prefix layouts in lockstep (the Zig side asserts offsets at comptime; the
/// Rust port adds the same assertions as tests when the port lands).
#[repr(C)]
#[derive(Debug)]
pub struct KiraNativeState {
    /// C: `uint64_t type_id`.
    pub type_id: u64,
    /// C: `void *payload` — raw byte buffer owned by the native runtime.
    pub payload: *mut c_void,
    /// C: `void *runtime_payload`.
    pub runtime_payload: *mut c_void,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Layout contracts with `runtime_helpers.c` for the native-bridge-owned
    /// structs (the `KiraBridgeValue` layout test lives in `kira-runtime-abi`
    /// next to the canonical definition).
    #[test]
    fn bridge_types_keep_the_c_layouts() {
        assert_eq!(size_of::<KiraArray>(), 3 * size_of::<usize>());
        assert_eq!(core::mem::offset_of!(KiraNativeState, type_id), 0);
        assert_eq!(core::mem::offset_of!(KiraNativeState, payload), 8);
        assert_eq!(core::mem::offset_of!(KiraNativeState, runtime_payload), 16);
    }
}
