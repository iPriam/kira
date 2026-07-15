//! `kira ffi autobind` driver: turns a native SDK (headers + libraries)
//! into generated Kira FFI bindings (`app/bindings/`).
//!
//! CRITICAL PATH: KG/kira-graphics (and every macOS/iOS runner app) depends
//! on FFI autobind restoration — graphics cannot come up in the Rust port
//! until this module family works end to end.
//!
//! Port target: kira-zig `kira_build/src/ffi_autobind.zig`.
