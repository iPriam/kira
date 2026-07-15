//! Apple Silicon hardware-breakpoint controller: BVR/BCR programming via
//! the mach thread-state API.
//!
//! Ported from kira-zig `packages/kira_debug/src/hw/darwin_arm64.zig`.
//! Known wall carried from Zig: cross-process
//! `thread_set_state(ARM_DEBUG_STATE64)` hangs on Apple Silicon; the VM
//! backend works, native hw breakpoints do not. No mach code at scaffold
//! time.
