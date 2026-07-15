//! WASM module assembly and validation helpers for the Web target.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_wasm_runtime/src/root.zig`: builds
//! the generated runtime wasm module (magic header, `kira.metadata` custom
//! section carrying app/surface JSON, type/function/export/code sections)
//! and validates candidate modules (`validateModule`, `isHeaderOnly` — a
//! header-only 8-byte module is the placeholder smoke artifact and must be
//! rejected as real output).
//!
//! `build_module` / `validate_module` / `is_header_only` logic lands with
//! the Web-target port.

// #![warn(missing_docs)] // enable once the port lands real code

/// Options for building the generated runtime module.
/// Zig: `ModuleOptions` (`app_name`, `surface`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModuleOptions {
    pub app_name: String,
    pub surface: String,
}

/// One exported wasm function descriptor.
/// Zig: private `WasmExport` (`name`, `value`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmExport {
    pub name: String,
    pub value: u32,
}

/// The 8-byte wasm magic + version header every module starts with.
/// Zig: the literal `{ 0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00 }`.
pub const WASM_HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-wasm-runtime"
}
