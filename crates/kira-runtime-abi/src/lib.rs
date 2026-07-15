//! Runtime ABI shared across backends: values, bridge values, and runtime handles.
//!
//! Layer 0 of the Kira package graph.
//! Ported from kira-zig `packages/kira_runtime_abi`.

pub mod bridge_value;
pub mod callable;
pub mod calling;
pub mod executor;
pub mod handles;
pub mod module_ids;
pub mod symbols;
pub mod task;
pub mod value;

pub use bridge_value::{BridgePayload, BridgeString, BridgeValue, BridgeValueTag};
pub use callable::{
    NATIVE_CLOSURE_POINTER_MASK, NATIVE_CLOSURE_TAG_BIT, is_tagged_native_closure_pointer,
    tag_native_closure_pointer, untag_native_closure_pointer,
};
pub use calling::{CallingConvention, ExecutionMode, FunctionExecution};
pub use executor::Executor;
pub use handles::{ModuleHandle, RuntimeHandle};
pub use module_ids::{RuntimeLibraryId, RuntimeModuleId, RuntimeSymbolId};
pub use symbols::RuntimeSymbol;
pub use task::{Poll, PollFn, Task, TaskId, TaskState};
pub use value::{Value, ValueTag};
