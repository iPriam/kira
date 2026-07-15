//! C-ABI bridge types, byte-for-byte mirrors of the structs declared in
//! kira-zig `packages/kira_native_bridge/src/runtime_helpers.c`.
//!
//! These are the types LLVM-compiled Kira code and the VM both touch, so the
//! `#[repr(C)]` layouts here are load-bearing. They intentionally duplicate
//! the placeholder ABI types in `kira_vm_runtime::abi` for now — TODO(port):
//! both move to `kira-runtime-abi` (the layer-0 ABI crate) once it scaffolds,
//! leaving exactly one definition in the workspace.

use core::ffi::c_void;

/// C: `KiraBridgeString` (`{ const unsigned char *ptr; size_t len; }`).
/// Null `ptr` iff `len == 0`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct KiraBridgeString {
    pub ptr: *const u8,
    pub len: usize,
}

/// C: `KiraBridgePayload` (union).
#[repr(C)]
#[derive(Clone, Copy)]
pub union KiraBridgePayload {
    /// C: `int64_t integer`.
    pub integer: i64,
    /// C: `double float64`.
    pub float64: f64,
    /// C: `KiraBridgeString string`.
    pub string: KiraBridgeString,
    /// C: `uint8_t boolean`.
    pub boolean: u8,
    /// C: `uintptr_t raw_ptr`.
    pub raw_ptr: usize,
}

/// C: `KiraBridgeValue` (`{ uint8_t tag; uint8_t reserved[7]; payload; }`).
/// Tag values follow `KiraBridgeValueTag`: 0 void, 1 integer, 2 float,
/// 3 string, 4 boolean, 5 raw_ptr; unknown tags degrade to void.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct KiraBridgeValue {
    pub tag: u8,
    pub reserved: [u8; 7],
    pub payload: KiraBridgePayload,
}

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

    /// Layout contracts with `runtime_helpers.c` (and, for the array header,
    /// with the VM heap's `ArrayObject`).
    #[test]
    fn bridge_types_keep_the_c_layouts() {
        assert_eq!(size_of::<KiraBridgeValue>(), 24);
        assert_eq!(core::mem::offset_of!(KiraBridgeValue, payload), 8);
        assert_eq!(size_of::<KiraArray>(), 3 * size_of::<usize>());
        assert_eq!(core::mem::offset_of!(KiraNativeState, type_id), 0);
        assert_eq!(core::mem::offset_of!(KiraNativeState, payload), 8);
        assert_eq!(core::mem::offset_of!(KiraNativeState, runtime_payload), 16);
    }
}
