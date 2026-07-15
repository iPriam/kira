//! Hybrid module manifests and bridge descriptors shared by hybrid producers and consumers.
//!
//! Layer 3 of the Kira package graph.
//! Ported from kira-zig `packages/kira_hybrid_definition`.

pub mod bridge_descriptor;
pub mod module_manifest;
pub mod runtime_contracts;
pub mod symbol_links;

pub use bridge_descriptor::{BridgeDescriptor, BridgeId, SymbolId};
pub use module_manifest::{
    FunctionManifest, HybridModuleManifest, OwnershipMode, TypeRef, TypeRefKind,
};
pub use runtime_contracts::RuntimeContract;
pub use symbol_links::SymbolLink;
