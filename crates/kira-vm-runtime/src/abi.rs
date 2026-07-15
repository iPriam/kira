//! Runtime-ABI value types, re-exported from `kira-runtime-abi`.
//!
//! The C-ABI layouts (`BridgeValue` family) and the owned `Value` model are
//! owned by the layer-0 `kira-runtime-abi` crate — exactly one definition in
//! the workspace; the layout tests live next to the types there. This module
//! only pins the VM-facing names.

pub use kira_runtime_abi::{
    BridgePayload, BridgeString, BridgeValue, BridgeValueTag, Value, ValueTag,
};
