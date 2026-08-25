//! Why a hybrid program could not be loaded or run.

use std::path::PathBuf;

use kira_hybrid_definition::ManifestDecodeError;

/// Why a hybrid program could not be loaded or run.
#[derive(Debug, thiserror::Error)]
pub enum HybridError {
    /// The bundle is a library, so there is no entrypoint to run.
    ///
    /// A hybrid library loads, validates, and binds its native half exactly as
    /// a hybrid application does — it just cannot be *started*, because a
    /// library is entered by its consumer one call at a time.
    #[error("this hybrid bundle is a library: it has no entrypoint to run")]
    NoEntrypoint,
    /// An artifact named by the manifest could not be read.
    #[error("cannot read `{path}`: {source}")]
    Io {
        /// The artifact that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The manifest itself is not a manifest this build can read.
    #[error("cannot decode the hybrid manifest `{path}`: {source}")]
    Manifest {
        /// The manifest that could not be decoded.
        path: PathBuf,
        /// Why decoding failed.
        #[source]
        source: ManifestDecodeError,
    },
    /// The bytecode half is not a module this build can read.
    #[error("cannot decode the bytecode half `{path}`: {source}")]
    Bytecode {
        /// The module that could not be decoded.
        path: PathBuf,
        /// Why decoding failed.
        #[source]
        source: kira_bytecode::module::ModuleDecodeError,
    },
    /// The bytecode half decoded but is not runnable.
    #[error("the bytecode half is not runnable: {0}")]
    Program(#[source] kira_vm_runtime::VmError),
    /// The shared library could not be loaded.
    #[error("cannot load the native half `{path}`: {source}")]
    Library {
        /// The library that could not be loaded.
        path: PathBuf,
        /// The underlying loader failure.
        #[source]
        source: libloading::Error,
    },
    /// A self-hosted native half could not be bound from this process image.
    #[error("cannot bind the native half from this process image: {0}")]
    SelfLibrary(#[source] kira_dynamic_ffi::FfiError),
    /// A declared foreign library or bundled Libffi runtime could not be loaded.
    #[error(transparent)]
    Foreign(#[from] kira_dynamic_ffi::ForeignLibraryError),
    /// A bundled Libffi closure could not be prepared.
    #[error(transparent)]
    Libffi(#[from] kira_libffi::LibffiError),
    /// A symbol the host must resolve is absent from the loaded library.
    ///
    /// A linker pulls only *referenced* members out of an archive, so a library
    /// that never happened to call one of these carries no definition of it.
    /// The link step forces each one in by name; this error means it did not.
    #[error(
        "the native half `{path}` does not export `{symbol}`; it was linked \
         without forcing the runtime's exported symbols in (this is a compiler \
         bug, not a program error)"
    )]
    MissingSymbol {
        /// The library that lacks the symbol.
        path: PathBuf,
        /// The symbol that could not be resolved.
        symbol: String,
        /// The underlying loader failure.
        #[source]
        source: libloading::Error,
    },
    /// The manifest and the bytecode half disagree about the program.
    ///
    /// The two artifacts are written by one build and are meant to describe one
    /// program. Disagreeing means the bundle is mismatched — a stale `.kbc`
    /// beside a fresh `.khm`, most likely — and running it would marshal against
    /// the wrong signature.
    #[error("the hybrid manifest and the bytecode half disagree: {0}")]
    Mismatch(String),
    /// A parameter mode the manifest records is outside the hybrid crossing
    /// contract.
    ///
    /// The compiler records a read-only borrow of a heap value as an owned
    /// crossing copy. A hand-edited or stale manifest that records `Borrow` for
    /// a `String` would instead ask the native trampoline to retain the
    /// caller's handle while the generated body releases every string
    /// parameter. Rejecting it at load keeps that mismatch from becoming a
    /// double free at the first crossing.
    #[error(
        "function `{function}` takes parameter {index} as `{ownership:?}`, which \
         this runtime does not support for `String` (read-only borrows cross as \
         owned copies)"
    )]
    UnsupportedOwnership {
        /// The function whose signature cannot be honoured.
        function: String,
        /// Which parameter, by position.
        index: usize,
        /// The mode the manifest recorded.
        ownership: kira_runtime_abi::Ownership,
    },
    /// The program trapped while running.
    #[error("{0}")]
    Trap(#[source] kira_vm_runtime::VmError),
}
