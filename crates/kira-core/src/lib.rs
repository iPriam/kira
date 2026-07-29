//! Foundational shared types, ids, identifiers, and error primitives used by every Kira crate, plus the structured logging module.
//!
//! Layer 0 of the Kira package graph.

pub mod errors;
pub mod identifiers;
pub mod ids;
pub mod log;
pub mod symbol;
pub mod types;

pub use errors::CommonError;
pub use identifiers::sanitize_kira_identifier;
pub use ids::{BridgeId, LibraryId, ModuleId, SymbolId};
pub use symbol::{Interner, Names, Symbol};
pub use types::Version;
