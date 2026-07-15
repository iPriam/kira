//! Build request, target, and artifact definitions shared by build producers and consumers.
//!
//! Layer 5 of the Kira package graph.
//! Ported from kira-zig `packages/kira_build_definition` (78 LOC, ported fully).

pub mod artifact;
pub mod build_request;
pub mod build_result;
pub mod build_target;

pub use artifact::{Artifact, ArtifactKind};
pub use build_request::BuildRequest;
pub use build_result::BuildResult;
pub use build_target::{BuildTarget, ExecutionTarget, TargetCapabilities, TargetEnvironment};
