//! Hardware breakpoint controllers, one per platform/arch, behind the
//! `HwBreakpointController` vtable in `controller`.
//!
//! Ported from kira-zig `packages/kira_debug/src/hw/`. Off-platform
//! controllers are cfg-gated in the port (Zig reached them only through a
//! comptime switch). NO ptrace/mach code at scaffold time.

pub mod controller;
pub mod darwin_arm64;
pub mod darwin_arm64_mach;
pub mod darwin_arm64_regs;
pub mod darwin_x86_64;
pub mod linux_arm64;
pub mod linux_x86_64;
pub mod software_trap;
pub mod windows;
pub mod windows_platform;
pub mod windows_regs;
