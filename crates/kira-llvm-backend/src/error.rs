//! Errors reported while lowering or linking native code.

use std::path::PathBuf;

use kira_toolchain::LlvmDiscoveryError;

use crate::link::LinkError;

/// What went wrong producing native code.
#[derive(Debug, thiserror::Error)]
pub enum LlvmError {
    /// A frontend invariant was violated before LLVM lowering.
    #[error("{what} reached the LLVM backend (this is a compiler bug)")]
    Internal {
        /// What was found, named as the invariant that did not hold.
        what: String,
    },
    /// A `break`/`continue` reached codegen with no enclosing loop, which
    /// analysis is supposed to have rejected.
    #[error(
        "a `break`/`continue` reached the LLVM backend outside a loop (this is a compiler bug)"
    )]
    JumpOutsideLoop,
    /// A read through an `@FFI.Pointer` named a member the target's C layout
    /// does not describe as a loadable scalar, which analysis is supposed to
    /// have rejected.
    #[error("a read of C-layout member {member} reached the LLVM backend (this is a compiler bug)")]
    ForeignMemberMissing {
        /// The member index the read asked for.
        member: u32,
    },
    /// A native library build did not name an archive or shared-library output.
    #[error("a native library build needs an archive path or a shared-library path")]
    MissingLibraryOutput,
    /// A hybrid native half did not name its shared-library output.
    #[error("a hybrid native build needs a shared-library path")]
    MissingHybridLibraryPath,
    /// A whole-program native live build did not name its shared-library output.
    #[error("a native live build needs a shared-library path")]
    MissingNativeLiveLibraryPath,
    /// No usable LLVM installation was found.
    #[error(transparent)]
    Discovery(#[from] LlvmDiscoveryError),
    /// The native FFI runtime could not be bundled into a native artifact.
    #[error(transparent)]
    FfiRuntime(#[from] kira_libffi::LibffiError),
    /// This compiler was built against a managed LLVM carrying no WebAssembly
    /// code generator, so it can emit for every device except the Web.
    #[error(
        "this compiler was built against a managed LLVM without the WebAssembly \
         code generator, so it cannot emit for the Web; install a bundle built \
         with the targets `llvm-metadata.toml` pins (`knvm install-llvm --force`) \
         and rebuild the compiler against it"
    )]
    WasmTargetMissing,
    /// This compiler was built against a managed LLVM carrying no code
    /// generator for the requested target's architecture.
    ///
    /// The counterpart of [`LlvmError::WasmTargetMissing`], and reported the
    /// same way and for the same reason: what a bundle can emit for is decided
    /// when the bundle is built, so a cross target it was not built with has to
    /// be refused by name at the point it is asked for. Every bundle published
    /// before `llvm-metadata.toml` named X86 and AArch64 outright carries only
    /// the generator of the machine that built it, and this is what those
    /// bundles say when asked for anything else.
    #[error(
        "this compiler was built against a managed LLVM without the {generator} \
         code generator, so it cannot emit code for `{target}`; install a bundle \
         built with the targets `llvm-metadata.toml` pins \
         (`knvm install-llvm --force`) and rebuild the compiler against it"
    )]
    TargetCodeGeneratorMissing {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// LLVM's name for the code generator that architecture needs.
        generator: &'static str,
    },
    /// The requested target names an architecture Kira has no code generator
    /// for, whatever the linked bundle carries.
    #[error(
        "`{target}` names architecture `{arch}`, which Kira has no LLVM code \
         generator for; the architectures it can emit for are `x86_64`, `x86`, \
         and `aarch64`"
    )]
    TargetArchitectureUnknown {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// Its architecture component.
        arch: String,
    },
    /// Two declarations want the same symbol name with different signatures —
    /// a foreign import colliding with a maths declaration, or two imports
    /// of one C symbol at different types. One flat namespace cannot hold
    /// both, and calling through the wrong signature fails module
    /// verification far from the line that caused it.
    #[error(
        "the symbol `{symbol}` is already declared with a different signature; \
         a foreign import and a builtin maths call are competing for one name"
    )]
    SymbolCollision {
        /// The symbol both declarations want.
        symbol: String,
    },
    /// LLVM refused the normalized triple for a target whose code generator is
    /// linked and registered.
    #[error(
        "LLVM does not recognize `{triple}`, the toolchain spelling of target \
         `{target}`"
    )]
    TargetTripleUnknown {
        /// The target that was asked for, in Kira's `arch-os-abi` spelling.
        target: String,
        /// The `arch-vendor-os-abi` triple LLVM was asked to resolve.
        triple: String,
    },
    /// The managed clang refused the generated C shim — always a backend bug,
    /// since Kira wrote every line of it.
    ///
    /// The source path is named rather than the text inlined: the file is left
    /// on disk beside the object, so the diagnostic points at something that can
    /// be read and compiled by hand.
    #[error("the managed clang refused the generated foreign shim `{source_path}`:\n{stderr}", source_path = source_path.display())]
    ShimUncompilable {
        /// The generated C file, left in place for inspection.
        source_path: PathBuf,
        /// The compiler's diagnostics.
        stderr: String,
    },
    /// Lowering produced a module LLVM rejected — always a backend bug.
    #[error("LLVM rejected the generated module (this is a compiler bug): {0}")]
    InvalidModule(String),
    /// LLVM could not emit an object for this target.
    #[error("LLVM could not emit an object file: {0}")]
    Emit(String),
    /// Linking the native executable failed.
    #[error(transparent)]
    Link(#[from] LinkError),
    /// An artifact path could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

impl LlvmError {
    /// An invariant the frontend proves, found not to hold.
    #[must_use]
    pub fn internal(what: impl Into<String>) -> Self {
        LlvmError::Internal { what: what.into() }
    }
}
