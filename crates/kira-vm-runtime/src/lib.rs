//! The Kira VM: bytecode interpreter, opcodes, hooks, and the debug controller.
//!
//! Layer 4 of the Kira package graph.
//! Ported from kira-zig `packages/kira_vm_runtime`. The module tree mirrors
//! the Zig file split one-to-one (test-only files excluded); this crate is the
//! future `unsafe` core of the runtime, currently scaffolded in a safe shape.

// #![warn(missing_docs)] // enable once the port lands real code

pub mod abi;
pub mod builtins;
pub mod module_loader;
pub mod native_layout;
pub mod opcodes;
pub mod ownership;
pub mod vm;
pub mod vm_construct_any;
pub mod vm_debug;
pub mod vm_ffi;
pub mod vm_helpers;
pub mod vm_interpreter;
pub mod vm_interpreter_fused;
pub mod vm_interpreter_native_state;
pub mod vm_interpreter_prologue;
pub mod vm_interpreter_strings;
pub mod vm_interpreter_tasks;
pub mod vm_native_bridge;
pub mod vm_native_closure_bridge;
pub mod vm_native_layout_destroy;
pub mod vm_prepare;
pub mod vm_reload;
pub mod vm_slot_utils;
pub mod vm_struct_copy;
pub mod vm_tasks;
pub mod vm_types;
pub mod vm_value_clone;
pub mod vm_values;

pub use abi::{BridgePayload, BridgeString, BridgeValue, BridgeValueTag, Value};
pub use ownership::{
    ArrayObject, ClosureObject, Heap, HeapStats, ObjectKind, ObjectOrigin, ObjectRecord,
    PointerObjectMap, StructFieldsObject,
};
pub use vm::Vm;
pub use vm_types::{ExportedNativeClosure, NativeLayoutStats, NativeStateBox};

/// Returns this crate's name (scaffold smoke check).
pub fn crate_name() -> &'static str {
    "kira-vm-runtime"
}
