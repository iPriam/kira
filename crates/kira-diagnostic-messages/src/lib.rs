//! Central catalog of diagnostic codes, domains, compiler phases, and message builders.
//!
//! Layer 0 of the Kira package graph.

pub mod backend_messages;
pub mod cli_messages;
pub mod compiler_bug_messages;
pub mod compiler_phase;
pub mod diagnostic_code;
pub mod diagnostic_domain;
pub mod diagnostic_message;
pub mod package_messages;
pub mod registry;
pub mod toolchain_messages;

pub use compiler_phase::CompilerPhase;
pub use diagnostic_code::DiagnosticCode;
pub use diagnostic_domain::DiagnosticDomain;
pub use diagnostic_message::{MessageArgs, build};
pub use registry::{CodeFamily, RegisteredCode};
