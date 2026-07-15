//! Ownership mode carried on parameters, loads, and closure captures.
//!
//! Ported from kira-zig `kira_bytecode/src/ownership_mode.zig`. The `u8`
//! discriminants are serialized into KBC containers and must never shift.

/// How a value crosses a binding/call boundary (Zig `OwnershipMode`, `enum(u8)`).
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OwnershipMode {
    /// Zig `.owned = 0`.
    #[default]
    Owned = 0,
    /// Zig `.borrow_read = 1`.
    BorrowRead = 1,
    /// Zig `.borrow_mut = 2`.
    BorrowMut = 2,
    /// Zig `.move = 3`.
    Move = 3,
    /// Zig `.copy = 4`.
    Copy = 4,
}
