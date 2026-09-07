//! The Kira VM: bytecode interpreter and runtime.
//!
//! Layer 4 of the Kira package graph.
//!
//! - **Portable core.** No filesystem, process, thread, or dynamic-loading
//!   calls; the crate compiles for `wasm32-unknown-unknown`. It consumes a
//!   [`Module`](kira_bytecode::Module) and talks to the world only through the
//!   [`HostCapabilities`](kira_runtime_abi::HostCapabilities) trait supplied by
//!   the embedder.
//! - **Affine strings.** Strings live on a heap with drop accounting; a clean
//!   run reclaims every allocation ([`HeapStats::current`] is 0 at exit).
//! - **Match-in-loop.** Dispatch is a single `match` over decoded instructions.
//!
//! # Two ways in, and why there are two
//!
//! [`execute`] and [`Program`] run a *program*: each call gets its own heap, and
//! that heap is gone when the call ends. [`Instance`] runs a *library*: one heap
//! for the instance's whole life, with a root table naming the objects the
//! consumer still holds. A library needs the second because Kira has no globals,
//! so an object returned by one call has nowhere else to live until the next
//! one. See [`instance`] for what "balanced" means once a heap outlives a call.

pub mod debug;
pub mod error;
pub mod fiber;
pub mod instance;
pub mod interp;
#[cfg(not(target_family = "wasm"))]
pub mod main_thread;
pub mod profile;
pub mod value;

pub use debug::{
    KIRA_VM_DEBUG_ACTIVE, KiraVmDebugFrame, KiraVmDebugState, KiraVmDebugValue, VmLldbBreakpoint,
    VmLldbObserver, format_debug_state, kira_vm_debug_dump, kira_vm_debug_probe,
};
pub use error::{NativeStateOperation, VmError};
pub use fiber::{Fiber, FiberStep};
pub use instance::{Instance, RootId};
pub use interp::{Program, RunOutcome, execute, execute_with_debug};
#[cfg(not(target_family = "wasm"))]
pub use main_thread::{
    MainThreadRunner, execute_with_main_thread, execute_with_main_thread_debug,
    execute_with_main_thread_using, execute_with_main_thread_using_debug,
};
pub use value::{Heap, HeapStats, StrId, Value};

#[cfg(test)]
#[path = "compiler_tests.rs"]
mod compiler_tests;

#[cfg(test)]
#[path = "capacity_tests.rs"]
mod capacity_tests;

#[cfg(test)]
#[path = "foreign_tests.rs"]
mod foreign_tests;

#[cfg(test)]
#[path = "frame_cache_tests.rs"]
mod frame_cache_tests;

#[cfg(test)]
#[path = "native_state_tests.rs"]
mod native_state_tests;

#[cfg(test)]
#[path = "release_tests.rs"]
mod release_tests;

#[cfg(test)]
#[path = "vm_test_support.rs"]
mod vm_test_support;

#[cfg(test)]
#[path = "debug_tests.rs"]
mod debug_tests;

#[cfg(test)]
#[path = "native_seam_tests.rs"]
mod native_seam_tests;

#[cfg(test)]
#[path = "numeric_tests.rs"]
mod numeric_tests;

#[cfg(test)]
#[path = "program_tests.rs"]
mod program_tests;
