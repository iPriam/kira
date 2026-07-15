//! Binary serialization of bytecode modules — the KBC container format.
//!
//! Ported from kira-zig `kira_bytecode/src/serialization.zig`. This scaffold
//! carries the magic constants and version lineage; the codec itself is TODO.
//!
//! # Container version lineage
//!
//! Every container starts with a 4-byte magic. The writer always emits the
//! current version; the reader accepts the whole lineage, defaulting the
//! features an older container lacks:
//!
//! - `KBC0` / `KBC2` — pre-ownership containers (rejected by the modern reader
//!   only when they carry ownership-bearing features).
//! - `KBC1` — adds function param/return ownership.
//! - `KBC3` — adds closure-capture ownership and load ownership.
//! - `KBC4` — adds indirect-call (call_value) param ownership.
//! - `KBC5` — adds FFI metadata: declared parameter types plus the optional
//!   foreign binding, so the VM can dispatch direct FFI through LibFFI.
//! - `KBC6` — adds construct families.
//! - `KBC7` — KBC6 plus the appended `convert` opcode; container layout and
//!   every feature flag otherwise identical to KBC6.
//! - `KBC8` — KBC7 plus a trailing `unsigned` flag byte on
//!   divide/modulo/compare (unsigned division/remainder/ordering). Older
//!   containers omit it and default to signed.
//! - `KBC9` — KBC8 plus the appended `free_native_state` opcode
//!   (`nativeStateFree`).
//! - `KBCA` — KBC9 plus a trailing `moved` flag byte on `load_indirect`
//!   (checker-verified field move-out — Rust partial move). Also carries the
//!   `moved` flags on `native_state_field_get` and `array_get`.
//! - `KBCB` — KBCA plus a trailing `reinterpret` flag byte on `convert`
//!   (Float<->bits bitcast: `floatToBits` / `bitsToFloat`).
//! - `KBCC` — KBCB plus the appended async task opcodes (`task_spawn`,
//!   `task_spawn_ready`, `task_await`, `task_cancel`, `task_detach`,
//!   `task_yield`, `frame_get`, `frame_set`, `task_is_complete`,
//!   `task_sleep`).
//! - `KBCD` — KBCC plus a trailing debug section (source-file table +
//!   per-function PC->source locations and local names). The container up to
//!   the last function is byte-identical to KBCC; older files omit the
//!   section and default all debug info to empty.

use crate::module::Module;

/// Length of the KBC magic prefix.
pub const MAGIC_LEN: usize = 4;

/// Oldest accepted magic (pre-ownership).
pub const MAGIC_KBC0: &[u8; 4] = b"KBC0";
/// Adds function ownership.
pub const MAGIC_KBC1: &[u8; 4] = b"KBC1";
/// Pre-ownership sibling of KBC0.
pub const MAGIC_KBC2: &[u8; 4] = b"KBC2";
/// Adds closure/load ownership.
pub const MAGIC_KBC3: &[u8; 4] = b"KBC3";
/// Adds indirect-call ownership.
pub const MAGIC_KBC4: &[u8; 4] = b"KBC4";
/// Adds FFI metadata (param types + foreign binding).
pub const MAGIC_KBC5: &[u8; 4] = b"KBC5";
/// Adds construct families.
pub const MAGIC_KBC6: &[u8; 4] = b"KBC6";
/// Appends the `convert` opcode.
pub const MAGIC_KBC7: &[u8; 4] = b"KBC7";
/// Adds unsigned arith/compare flags.
pub const MAGIC_KBC8: &[u8; 4] = b"KBC8";
/// Appends `free_native_state`.
pub const MAGIC_KBC9: &[u8; 4] = b"KBC9";
/// Adds the `moved` (partial-move) flags.
pub const MAGIC_KBCA: &[u8; 4] = b"KBCA";
/// Adds the `convert.reinterpret` flag.
pub const MAGIC_KBCB: &[u8; 4] = b"KBCB";
/// Appends the async task opcodes.
pub const MAGIC_KBCC: &[u8; 4] = b"KBCC";
/// Appends the trailing debug section.
pub const MAGIC_KBCD: &[u8; 4] = b"KBCD";

/// The magic the writer emits (Zig `serialize` writes `"KBCD"`).
pub const CURRENT_MAGIC: &[u8; 4] = MAGIC_KBCD;

/// Errors produced by the KBC codec (Zig `error.InvalidBytecode` et al.).
#[derive(Debug, thiserror::Error)]
pub enum SerializationError {
    /// The container magic is unknown or the payload is malformed.
    #[error("invalid bytecode container")]
    InvalidBytecode,
}

/// Serializes `module` as a KBCD container (Zig `serialize`).
///
/// TODO(port): the full field-level codec (serialization_primitives.zig) and
/// the trailing debug section writer (serialization_debug.zig). Fused
/// (VM-internal) instructions must be rejected.
pub fn serialize(_module: &Module) -> Result<Vec<u8>, SerializationError> {
    todo!("port kira_bytecode/src/serialization.zig serialize")
}

/// Deserializes a KBC container, accepting the whole KBC0..KBCD lineage with
/// per-version feature defaults (Zig `deserialize`).
///
/// TODO(port): the full reader, including the version feature flags
/// (`has_ffi_metadata`, `has_unsigned_arith`, `has_load_indirect_moved`,
/// `has_convert_reinterpret`, `has_debug_section`, ...).
pub fn deserialize(_bytes: &[u8]) -> Result<Module, SerializationError> {
    todo!("port kira_bytecode/src/serialization.zig deserialize")
}
