//! Hot-reload support: function-id remapping for live closures across a
//! module swap, and retired-module lifetime rules (heap values borrow
//! type-name/string-literal bytes from retired module memory).
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/vm_reload.zig`.
//! Logic lands with the live/hot-swap port.
