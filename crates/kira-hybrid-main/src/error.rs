//! Why a hybrid library did not load, or a call to one did not run.
//!
//! One enum for both, for the reason the VM engine's [`kira_main::Error`] is one
//! enum: a generated wrapper's every method returns the same type, and a
//! consumer writing `?` should not have to know which layer a failure came out
//! of. What the layers are shows up in the *message*, not in the type.

use std::path::PathBuf;

use kira_runtime_abi::NativeResult;

/// Why a hybrid library could not be loaded or called.
#[derive(Debug, thiserror::Error)]
pub enum HybridMainError {
    /// The bytecode half, its export surface, or a call into it.
    ///
    /// Everything the VM engine can go wrong at goes wrong here too, because
    /// the bytecode half *is* a VM-engine library — the hybrid engine adds a
    /// native half beside it rather than replacing it.
    #[error(transparent)]
    Bytecode(#[from] kira_main::Error),
    /// The `.khm` manifest did not decode.
    #[error("the hybrid manifest is not readable: {0}")]
    Manifest(#[from] kira_hybrid_definition::ManifestDecodeError),
    /// The native half is not anywhere the search looked.
    ///
    /// Carries every path tried, in the order they were tried, because "not
    /// found" without the list is the least actionable thing a loader can say.
    /// See [`crate::locate`] for what the order means.
    #[error(
        "the native half of `{library}` is not where this program looked for it.\n\
         note: tried, in order: {}\n\
         note: set `{variable}` to point at it, or copy it beside this executable",
        .tried.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join(", ")
    )]
    NativeHalfMissing {
        /// The library whose native half is absent.
        library: String,
        /// The environment variable that overrides the search.
        variable: String,
        /// Every path tried, in order.
        tried: Vec<PathBuf>,
    },
    /// The native half was found but would not load or bind.
    #[error("the native half of `{library}` could not be loaded: {source}")]
    NativeHalf {
        /// The library whose native half failed.
        library: String,
        /// What the hybrid runtime said about it.
        ///
        /// Boxed because this is the widest failure the loader carries and
        /// every wrapper method returns this enum by value.
        #[source]
        source: Box<kira_hybrid_runtime::HybridError>,
    },
    /// The two halves do not describe the same program.
    ///
    /// Both are written by one build, so this is a stale-artifact report rather
    /// than a user error: a `.kbc` from one build beside a `.khm` from another.
    #[error(
        "the two halves of `{library}` do not agree: {reason}\n\
         note: rebuild with `kira build --backend hybrid`"
    )]
    Mismatch {
        /// The library whose halves disagree.
        library: String,
        /// What disagreed.
        reason: String,
    },
}

impl HybridMainError {
    /// Names a result the library returned under a tag its declared signature
    /// rules out.
    ///
    /// The generated wrapper's every method ends in this: it matches the arm its
    /// export's declared result type promises, and anything else is a library
    /// disagreeing with the surface the wrapper was generated from. Unreachable
    /// through a verified artifact, and typed anyway — a generator never gets to
    /// end its consumer's process.
    ///
    /// Delegated to [`kira_main::Error`] rather than restated, because the
    /// bytecode half *is* a VM-engine library and one wrong tag must read the
    /// same whichever engine a consumer built against. That sameness is not
    /// cosmetic: `consumer.rs` in the export-consumer crate is compiled
    /// unchanged against all three engines, and a method body that had to spell
    /// its error differently per engine would be a difference in the generated
    /// API — the one thing this feature promises there is not.
    pub fn unexpected_result(
        export: &str,
        expected: &'static str,
        found: &NativeResult,
    ) -> HybridMainError {
        HybridMainError::Bytecode(kira_main::Error::unexpected_result(export, expected, found))
    }
}
