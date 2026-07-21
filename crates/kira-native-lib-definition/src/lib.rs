//! Native library definitions: the model for per-target static archives and how
//! a foreign import resolves to one.
//!
//! Layer 3 of the Kira package graph.
//!
//! This crate is the pure model: it parses a structured [`TargetTriple`], holds
//! an unresolved [`NativeLibraryManifest`] and its resolved form, and answers
//! "which archive does this foreign import link against for this target?". It
//! performs no filesystem I/O of its own — path existence is supplied by an
//! injected predicate — so the resolution logic stays testable without disk and
//! the layering above (`kira-manifest` parses the TOML, `kira-project` reads the
//! files) owns the I/O.
//!
//! ## Interner ownership
//!
//! A [`ResolvedNativeLibraries`] catalog is keyed by an interned
//! [`kira_core::Symbol`], and it **owns the [`kira_core::Interner`]** those
//! symbols come from. A foreign import carries its library name as a `String`,
//! so a lookup interns that name into the catalog's own interner
//! ([`ResolvedNativeLibraries::intern_library`]) and then resolves the resulting
//! symbol ([`ResolvedNativeLibraries::resolve_import`]). Because build and lookup
//! share the one interner the catalog owns, the same name always maps to the
//! same symbol and there is no cross-interner mismatch to guard against.

mod catalog;
mod manifest;
mod triple;

pub use catalog::{ImportResolveError, ResolvedNativeLibraries};
// Re-exported so a caller can build a catalog without a direct `kira-core`
// dependency: [`ResolvedNativeLibraries::from_resolved`] takes ownership of one.
pub use kira_core::Interner;
pub use manifest::{
    NativeLibraryError, NativeLibraryManifest, NativeTargetRow, ResolvedNativeLibrary,
    ResolvedTargetRow,
};
pub use triple::{TargetTriple, TripleError};
