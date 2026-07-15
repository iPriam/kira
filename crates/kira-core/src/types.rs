//! Foundational shared value types.
//!
//! Mirrors kira-zig `packages/kira_core/src/types.zig`. The Zig `Allocator`
//! and `String` aliases have no Rust counterpart (ownership replaces them).

/// A semantic version triple (`major.minor.patch`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Version {
    /// Major version component.
    pub major: u16,
    /// Minor version component.
    pub minor: u16,
    /// Patch version component.
    pub patch: u16,
}
