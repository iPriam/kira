//! Public options and artifact records shared by native and hybrid builds.

use std::path::PathBuf;

use crate::NativeExportSurface;
use kira_native_lib_definition::NativeLinkInputs;

use crate::link::NativeBuildTarget;

#[derive(Debug, Clone, PartialEq)]
pub struct NativeBuildOptions {
    /// The module name recorded in the emitted artifacts.
    pub module_name: String,
    /// Where the object file is written.
    pub object_path: PathBuf,
    /// Where the linked executable is written, when one is requested.
    pub executable_path: Option<PathBuf>,
    /// Where the shared library is written, for a hybrid or native live build.
    pub shared_library_path: Option<PathBuf>,
    /// Where the static archive is written, for a library build.
    ///
    /// A Rust consumer links the archive rather than the dylib: it needs no
    /// deployment story — no question of where the library lives at load time or
    /// how it is found — because the code ends up inside the consumer's own
    /// binary.
    pub archive_path: Option<PathBuf>,
    /// What this library exports, for a library build.
    ///
    /// Empty for a program and for a hybrid half, which are entered another way
    /// entirely.
    pub exports: NativeExportSurface,
    /// Where the textual LLVM IR is written, when requested. A debugging aid:
    /// it is never an input to the codegen path.
    pub ir_path: Option<PathBuf>,
    /// The native runtime archive (`libkira_native_bridge.a`) to link against.
    pub runtime_archive: PathBuf,
    /// The resolved C link inputs that satisfy the program's `@FFI.Extern`
    /// imports: archives in link order plus the frameworks, system libraries,
    /// and linker flags declared beside them. Empty for a program with no
    /// foreign imports.
    pub foreign_link: NativeLinkInputs,
    /// Whether to optimize the emitted code.
    ///
    /// Optimizing a large module is the dominant cost of a native build — two
    /// minutes against seconds for the editor — so a development build leaves
    /// it off and a shipped one turns it on.
    pub optimize: bool,
    /// The imports whose library is absent on this target, by index.
    ///
    /// Their adapters return
    /// [`ForeignAdapterStatus::UNAVAILABLE_LIBRARY`](kira_runtime_abi::ForeignAdapterStatus::UNAVAILABLE_LIBRARY)
    /// without naming the C symbol, so a Direct3D binding compiled on macOS
    /// contributes no undefined reference to the link.
    pub unavailable_imports: Vec<usize>,
    /// Which machine this build emits and links for, and where that machine's
    /// system libraries live.
    ///
    /// [`NativeBuildTarget::host`] is the default and what every build that
    /// produces something *this* process loads must use. It decides three
    /// things at once, and they have to be one value rather than three because
    /// disagreeing is silent: the data layout the lowering computes offsets
    /// against, the code generator the object comes out of, and the machine the
    /// link line is aimed at.
    pub target: NativeBuildTarget,
    /// Which sanitizer instruments this build, if any.
    pub sanitize: Sanitize,
}

/// Which sanitizer a native build carries.
///
/// One value threaded from the command line to the emitted object and the
/// link line together, because the two halves are useless apart: an
/// instrumented object without the runtime fails to link, and the runtime
/// without instrumentation watches nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Sanitize {
    /// No instrumentation, the default.
    #[default]
    None,
    /// AddressSanitizer: every Kira function is instrumented and the link
    /// carries the managed bundle's ASan runtime — never a host compiler's.
    Address,
}

/// The artifacts a native build produced.
#[derive(Debug, Clone, PartialEq)]
pub struct NativeArtifacts {
    /// The emitted object file.
    pub object: PathBuf,
    /// The linked executable, when one was requested.
    pub executable: Option<PathBuf>,
    /// The linked static archive, for a library build.
    ///
    /// Self-contained: the runtime archive's members are inside it, so a
    /// consumer links one file and needs no arrangement with the Kira
    /// toolchain.
    pub archive: Option<PathBuf>,
    /// The linked shared library, for a library or native live build.
    ///
    /// Exclusive with [`NativeArtifacts::executable`] in practice: a program
    /// produces one and a library the other, which is what makes "this build
    /// produced a library" checkable rather than asserted.
    pub library: Option<PathBuf>,
    /// The textual LLVM IR dump, when one was requested.
    pub ir: Option<PathBuf>,
}
