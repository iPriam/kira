//! Native-to-VM bridge: runtime invokers and closure lifecycle plumbing.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_native_bridge`. The Zig package is
//! half Zig (`bridge.zig`, `trampoline.zig`, `symbol_resolver.zig`,
//! `graphics_loader.zig`, `async_runtime.zig`) and half C
//! (`runtime_helpers.c` + `.inc` files) compiled into every native build;
//! the Rust port keeps the C ABI identical (see `abi` / `exports`) while the
//! helper implementations become Rust.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod abi;
pub mod async_runtime;
pub mod bridge;
pub mod exports;
pub mod graphics_loader;
pub mod symbol_resolver;
pub mod trampoline;

pub use abi::{KiraArray, KiraBridgePayload, KiraBridgeString, KiraBridgeValue, KiraNativeState};
pub use bridge::NativeBridge;
pub use trampoline::{NativeTrampolineFn, Trampoline};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-native-bridge"
}
