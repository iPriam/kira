//! Native library definitions: the model for what a package declares about a C
//! library, and what a foreign import puts on the link line because of it.
//!
//! Layer 3 of the Kira package graph.
//!
//! This crate is the pure model: it parses a structured [`TargetTriple`], holds
//! a declared [`NativeLibrarySpec`] and its resolved form, and answers "what
//! does this foreign import link against for this target?". It
//! performs no filesystem I/O of its own — path existence is supplied by an
//! injected predicate — so the resolution logic stays testable without disk and
//! the layering above (`kira-manifest` decodes both declaration spellings,
//! `kira-project` reads the files) owns the I/O.
//!
//! ## One model, two spellings
//!
//! A package declares a native library inline in its `package.kira`
//! (`let nativeLibraries = [NativeLibrary { ... }]`) or in a
//! `NativeLibs/<name>.toml`. Both decode into [`NativeLibrarySpec`], so nothing
//! downstream behaves differently depending on where a library was written.
//!
//! ## An archive is not the whole answer
//!
//! A target row may name a static archive, a shared library, or no file at all:
//! a macOS row can be Apple frameworks and `-lobjc` only. So resolution yields a
//! [`ResolvedTargetRow`] carrying an optional artifact plus its
//! [`NativeLinkAttributes`], and a build gathers the rows its imports selected
//! into a [`NativeLinkInputs`] — the one thing every link path consumes.
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
mod resolved;
mod spec;
mod triple;

pub use catalog::{ImportResolveError, ResolvedNativeLibraries};
// Re-exported so a caller can build a catalog without a direct `kira-core`
// dependency: [`ResolvedNativeLibraries::from_resolved`] takes ownership of one.
pub use kira_core::Interner;
pub use resolved::{
    MissingArchive, NativeLibraryError, NativeLinkAttributes, NativeLinkInputs,
    ResolvedNativeLibrary, ResolvedTargetRow,
};
pub use spec::{
    AutobindMode, AutobindProfile, AutobindSpec, Availability, LinkMode, NativeArtifact,
    NativeHeaders, NativeLibrarySpec, NativeTargetSpec,
};
pub use triple::{TargetTriple, TripleError};
