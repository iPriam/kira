//! C-ABI bridge value layout shared between the VM and LLVM/native code.
//!
//! Ported from kira-zig `kira_runtime_abi/src/bridge_value.zig`. Every type
//! here is `#[repr(C)]` (Zig `extern struct` / `extern union`) — the layouts
//! are ABI and must match the Zig side byte-for-byte.

/// Bridge value discriminant (Zig `BridgeValueTag`, non-exhaustive `enum(u8)`).
///
/// Modeled as a transparent `u8` newtype instead of a Rust `enum` because the
/// Zig enum is open (`_`): foreign code may hand us any byte, and an
/// out-of-range Rust enum value would be UB. Unknown tags degrade to void on
/// conversion, matching the Zig `toValue` fallback.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BridgeValueTag(pub u8);

impl BridgeValueTag {
    /// Zig `.void` (`ValueTag.void`).
    pub const VOID: BridgeValueTag = BridgeValueTag(0);
    /// Zig `.integer` (`ValueTag.integer`).
    pub const INTEGER: BridgeValueTag = BridgeValueTag(1);
    /// Zig `.float` (`ValueTag.float`).
    pub const FLOAT: BridgeValueTag = BridgeValueTag(2);
    /// Zig `.string` (`ValueTag.string`).
    pub const STRING: BridgeValueTag = BridgeValueTag(3);
    /// Zig `.boolean` (`ValueTag.boolean`).
    pub const BOOLEAN: BridgeValueTag = BridgeValueTag(4);
    /// Zig `.raw_ptr` (`ValueTag.raw_ptr`).
    pub const RAW_PTR: BridgeValueTag = BridgeValueTag(5);
}

/// Borrowed string view crossing the bridge (Zig `BridgeString`, `extern struct`).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct BridgeString {
    /// Zig `ptr: ?[*]const u8` — null for the empty string.
    pub ptr: *const u8,
    /// Zig `len: usize`.
    pub len: usize,
}

/// Untagged payload of a [`BridgeValue`] (Zig `BridgePayload`, `extern union`).
///
/// Reading a field is only meaningful for the variant selected by the sibling
/// [`BridgeValueTag`]; conversion helpers own that `unsafe` (TODO: port
/// `fromValue`/`toValue`).
#[repr(C)]
#[derive(Clone, Copy)]
pub union BridgePayload {
    /// Zig `integer: i64`.
    pub integer: i64,
    /// Zig `float: f64`.
    pub float: f64,
    /// Zig `string: BridgeString`.
    pub string: BridgeString,
    /// Zig `boolean: u8` (0 = false, nonzero = true).
    pub boolean: u8,
    /// Zig `raw_ptr: usize`.
    pub raw_ptr: usize,
}

/// The bridge value cell (Zig `BridgeValue`, `extern struct`).
///
/// Layout: 1 tag byte, 7 reserved padding bytes (explicit on the Zig side so
/// the payload is 8-aligned on both ends), then the 16-byte payload union.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct BridgeValue {
    /// Zig `tag: BridgeValueTag`.
    pub tag: BridgeValueTag,
    /// Zig `reserved: [7]u8 = .{0} ** 7` — explicit padding, always zero.
    pub reserved: [u8; 7],
    /// Zig `payload: BridgePayload = .{ .raw_ptr = 0 }`.
    pub payload: BridgePayload,
}

impl BridgeValue {
    /// A void bridge value (Zig default: `.{ .tag = .void, .payload = .{ .raw_ptr = 0 } }`).
    pub const fn void() -> BridgeValue {
        BridgeValue {
            tag: BridgeValueTag::VOID,
            reserved: [0; 7],
            payload: BridgePayload { raw_ptr: 0 },
        }
    }
}

// TODO(port): `fromValue` / `toValue` conversions between `Value` and
// `BridgeValue` (kira-zig bridge_value.zig `fromValue`/`toValue`), including
// the "invalid tag degrades to void" rule.

#[cfg(test)]
mod tests {
    use super::*;

    /// Mirrors the layout contract with `KiraBridgeValue` in kira-zig
    /// `packages/kira_native_bridge/src/runtime_helpers.c`: 1-byte tag,
    /// 7 reserved padding bytes, 16-byte payload union, 8-aligned payload.
    #[test]
    fn bridge_value_layout_matches_the_c_abi() {
        assert_eq!(size_of::<BridgeString>(), 2 * size_of::<usize>());
        assert_eq!(size_of::<BridgePayload>(), 16);
        assert_eq!(size_of::<BridgeValue>(), 24);
        assert_eq!(core::mem::offset_of!(BridgeValue, tag), 0);
        assert_eq!(core::mem::offset_of!(BridgeValue, payload), 8);
    }
}
