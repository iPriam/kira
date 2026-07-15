//! Owned runtime value model shared by the VM and the native bridge.
//!
//! Ported from kira-zig `kira_runtime_abi/src/value.zig`.

/// Discriminant of a runtime [`Value`] (Zig `ValueTag`, `enum(u8)`).
///
/// The numeric values are ABI: [`crate::bridge_value::BridgeValueTag`] reuses
/// them byte-for-byte across the C bridge.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueTag {
    /// Zig `.void`.
    Void = 0,
    /// Zig `.integer`.
    Integer = 1,
    /// Zig `.float`.
    Float = 2,
    /// Zig `.string`.
    String = 3,
    /// Zig `.boolean`.
    Boolean = 4,
    /// Zig `.raw_ptr`.
    RawPtr = 5,
}

/// A runtime value (Zig `Value`, a tagged union over [`ValueTag`]).
///
/// The Zig `string` payload is a borrowed `[]const u8`; the Rust port owns its
/// bytes (no lifetimes in model types per the port rules).
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    /// Zig `.void`.
    #[default]
    Void,
    /// Zig `.integer: i64`.
    Integer(i64),
    /// Zig `.float: f64`.
    Float(f64),
    /// Zig `.string: []const u8` (owned here).
    String(String),
    /// Zig `.boolean: bool`.
    Boolean(bool),
    /// Zig `.raw_ptr: usize`.
    RawPtr(usize),
}

impl Value {
    /// Returns the [`ValueTag`] discriminant for this value.
    pub fn tag(&self) -> ValueTag {
        match self {
            Value::Void => ValueTag::Void,
            Value::Integer(_) => ValueTag::Integer,
            Value::Float(_) => ValueTag::Float,
            Value::String(_) => ValueTag::String,
            Value::Boolean(_) => ValueTag::Boolean,
            Value::RawPtr(_) => ValueTag::RawPtr,
        }
    }
}
