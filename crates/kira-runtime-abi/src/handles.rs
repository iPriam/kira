//! Opaque runtime and module handles.
//!
//! Ported from kira-zig `kira_runtime_abi/src/handles.zig`.

/// Opaque handle to a live runtime instance (Zig `RuntimeHandle`, `enum(u32)`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeHandle(pub u32);

impl RuntimeHandle {
    /// Zig `.invalid = 0`.
    pub const INVALID: RuntimeHandle = RuntimeHandle(0);
}

/// Opaque handle to a loaded module (Zig `ModuleHandle`, `enum(u32)`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ModuleHandle(pub u32);

impl ModuleHandle {
    /// Zig `.invalid = 0`.
    pub const INVALID: ModuleHandle = ModuleHandle(0);
}
