//! Placeholder runtime-ABI value types.
//!
//! TODO(port): these are the C-ABI value types owned by `kira-runtime-abi`
//! (kira-zig `packages/kira_runtime_abi/src/value.zig` and
//! `bridge_value.zig`). They live here temporarily so the VM heap scaffolds
//! are self-contained; move them to `kira-runtime-abi` and re-export from
//! there once that crate is scaffolded, keeping exactly one definition of the
//! ABI layout in the workspace.

/// Discriminant shared by `Value` and `BridgeValue`.
/// Zig: `ValueTag` / `BridgeValueTag` (`enum(u8)`, same integer values).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BridgeValueTag {
    Void = 0,
    Integer = 1,
    Float = 2,
    String = 3,
    Boolean = 4,
    RawPtr = 5,
}

/// C-ABI string view: pointer + length, null pointer for the empty string.
/// Zig: `BridgeString` (`extern struct { ptr: ?[*]const u8, len: usize }`);
/// C: `KiraBridgeString` in `runtime_helpers.c`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BridgeString {
    /// Zig: `ptr: ?[*]const u8`. Null iff `len == 0`.
    pub ptr: *const u8,
    /// Zig: `len: usize`.
    pub len: usize,
}

/// C-ABI value payload.
/// Zig: `BridgePayload` (`extern union`); C: `KiraBridgePayload`.
#[repr(C)]
#[derive(Clone, Copy)]
pub union BridgePayload {
    /// Zig: `integer: i64`.
    pub integer: i64,
    /// Zig: `float: f64`.
    pub float: f64,
    /// Zig: `string: BridgeString`.
    pub string: BridgeString,
    /// Zig: `boolean: u8` (0 or 1).
    pub boolean: u8,
    /// Zig: `raw_ptr: usize`.
    pub raw_ptr: usize,
}

/// The C-ABI value crossing the native bridge.
/// Zig: `BridgeValue` (`extern struct`); C: `KiraBridgeValue`.
///
/// Layout invariant (shared with `runtime_helpers.c`): 1-byte tag,
/// 7 reserved padding bytes, then the 16-byte payload union. An invalid tag
/// degrades to void on conversion — it never traps.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BridgeValue {
    /// Zig: `tag: BridgeValueTag`. Stored as a raw `u8` because foreign
    /// writers may hand us out-of-range tags, which must stay representable.
    pub tag: u8,
    /// Zig: `reserved: [7]u8`, always zero.
    pub reserved: [u8; 7],
    /// Zig: `payload: BridgePayload`.
    pub payload: BridgePayload,
}

/// The VM's working value.
/// Zig: `Value` (`union(ValueTag)` in `kira_runtime_abi/src/value.zig`).
///
/// The Zig `string` payload is a borrowed `[]const u8` whose bytes are owned
/// by the heap registry or a loaded module's constant pool; the scaffold
/// carries the same ptr+len view via `BridgeString`. TODO(port): revisit the
/// borrow model (lifetime, interned id, or raw view) when the interpreter
/// lands.
#[derive(Debug, Clone, Copy)]
pub enum Value {
    /// Zig: `.void`.
    Void,
    /// Zig: `.integer: i64`.
    Integer(i64),
    /// Zig: `.float: f64`.
    Float(f64),
    /// Zig: `.string: []const u8` (borrowed bytes).
    String(BridgeString),
    /// Zig: `.boolean: bool`.
    Boolean(bool),
    /// Zig: `.raw_ptr: usize` (a key into the heap registry, or a foreign
    /// pointer the registry does not know — probes must tolerate misses).
    RawPtr(usize),
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the layout contract with `KiraBridgeValue` in
    /// `runtime_helpers.c`: 1-byte tag + 7 reserved + 16-byte payload union.
    #[test]
    fn bridge_value_layout_matches_the_c_abi() {
        assert_eq!(size_of::<BridgeString>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<BridgePayload>(), 16);
        assert_eq!(size_of::<BridgeValue>(), 24);
        assert_eq!(core::mem::offset_of!(BridgeValue, tag), 0);
        assert_eq!(core::mem::offset_of!(BridgeValue, payload), 8);
    }
}
