//! Locates and loads the kira-graphics host library for UI apps, keeping
//! one process-wide handle so sokol/window state survives module swaps.
//!
//! Ported from kira-zig
//! `packages/kira_native_bridge/src/graphics_loader.zig`.
//! Logic lands with the platform-runner port.
