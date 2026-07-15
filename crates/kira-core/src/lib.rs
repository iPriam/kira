//! Foundational shared types, ids, identifiers, and error primitives used by every Kira crate; includes the structured logging module (merged from kira_log).
//!
//! Layer 0 of the Kira package graph.
//! Ported from kira-zig `packages/kira_core`.

pub mod errors;
pub mod identifiers;
pub mod ids;
pub mod log;
pub mod symbol;
pub mod types;

pub use errors::CommonError;
pub use identifiers::sanitize_kira_identifier;
pub use ids::{BridgeId, LibraryId, ModuleId, SymbolId};
pub use symbol::{Interner, Symbol};
pub use types::Version;
