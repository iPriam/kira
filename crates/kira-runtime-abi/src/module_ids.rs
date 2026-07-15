//! Runtime-facing aliases for the core module/symbol/library ids.
//!
//! Ported from kira-zig `kira_runtime_abi/src/module_ids.zig`, where these are
//! aliases of `kira_core.ModuleId` / `SymbolId` / `LibraryId`.
//!
//! TODO(port): replace these local placeholder newtypes with re-exports of the
//! `kira-core` id types once that crate defines them (it is still an empty
//! skeleton at scaffold time).

/// Runtime module id (Zig `RuntimeModuleId = core.ModuleId`). Placeholder newtype.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeModuleId(pub u32);

/// Runtime symbol id (Zig `RuntimeSymbolId = core.SymbolId`). Placeholder newtype.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeSymbolId(pub u32);

/// Runtime library id (Zig `RuntimeLibraryId = core.LibraryId`). Placeholder newtype.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RuntimeLibraryId(pub u32);
