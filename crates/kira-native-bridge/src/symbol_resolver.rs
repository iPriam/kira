//! Resolves descriptor symbol names to `NativeTrampolineFn` pointers in a
//! loaded library, with the platform-specific name-mangling fallbacks
//! (leading-underscore retry on Mach-O).
//!
//! Ported from kira-zig
//! `packages/kira_native_bridge/src/symbol_resolver.zig`.
//! Logic lands with the bridge port.
