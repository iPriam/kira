//! Native runtime support and the native<->VM bridge surface.
//!
//! Layer 4 of the Kira package graph. This crate is native-only and lives
//! outside the portable VM core: it is compiled to a static archive
//! (`libkira_native_bridge.a`) and linked into every native executable the
//! LLVM backend produces.
//!
//! Today it provides the [`runtime`] support library — the stable C-ABI helper
//! symbols that LLVM/native-lowered Kira code calls for `print` and string
//! values. Because those helpers format with the same Rust standard library the
//! VM uses, `print` output is identical byte-for-byte across `kira run` (VM) and
//! `kira build --backend llvm` (native). The hybrid native<->runtime bridge
//! (trampolines, the installed runtime invoker) is designed fresh alongside the
//! hybrid runtime and will live beside it here.

pub mod runtime;

pub use runtime::{KStr, KiraString};
