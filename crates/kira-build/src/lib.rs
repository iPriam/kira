//! The build system: drives the frontend, then packages what a consumer needs.
//!
//! Layer 7 of the Kira package graph.
//!
//! # What lives here and why
//!
//! Two things, and they are the same thing seen from either end:
//!
//! - [`frontend`] turns a path to a `.kira` file into an
//!   [`IrProgram`](kira_ir::IrProgram) plus package-resolution and frontend
//!   diagnostics. For package-owned files it resolves transitive path
//!   dependencies and loads their modules before semantics; library packages
//!   additionally compile every source below `app/`, while a bare source file
//!   keeps the standalone behavior. It is a *library* function rather than CLI
//!   code because `kirac` is not the only thing that compiles Kira: a consumer
//!   crate's `build.rs` builds the library it embeds, and it must reach the
//!   identical pipeline rather than a second one that drifts from it.
//! - [`library`] takes that program and produces what a Rust consumer actually
//!   depends on: the `.kbc` artifact, and a generated wrapper crate around it
//!   ([`wrapper`]).
//!
//! # The generated crate is code
//!
//! [`wrapper::generate`] is pure — spec in, file contents out, no filesystem —
//! so what it emits can be asserted on directly. Everything it emits is held to
//! the same bar as hand-written code here: it is `rustfmt`-shaped, it carries a
//! doc comment on every public item, and it contains no `unsafe`. The VM engine
//! needs none — the wrapper embeds bytecode and calls `kira-main` through safe
//! Rust — which is exactly why this is the engine that a consumer can build on a
//! machine with no LLVM and no linker.
//!
//! FFI autobind — generating *Kira* bindings for a native library, the opposite
//! direction — is designed in here too, and is not built yet.

pub mod callgraph;
pub mod frontend;
pub mod hybrid;
pub mod library;
pub mod native;
mod shader;
pub mod wrapper;

pub use frontend::{Compiled, FrontendError, compile};
pub use hybrid::{
    HybridLibraryArtifacts, HybridLibraryError, HybridLibraryOptions, build_hybrid_library,
    check_library as check_hybrid_library, manifest as hybrid_manifest,
};
// Re-exported rather than restated: a consumer's `build.rs` that reaches the
// generated wrapper without running the generated `build.rs` still has to name
// the same platform libraries, and there is one list.
pub use kira_llvm_backend::{PLATFORM_LINK_LISTS, PlatformLinkList, host_link_list, link_list_for};
pub use library::{
    LibraryArtifacts, LibraryBuildError, LibraryBuildOptions, build_library, toolchain_root,
};
pub use native::{
    NativeLibraryArtifacts, NativeLibraryError, NativeLibraryOptions, archive_file_name,
    build_native_library, export_surface,
};
pub use wrapper::{
    GeneratedCrate, GeneratedFile, NativeWrapperSpec, WrapperSpec, generate, generate_hybrid,
    generate_native,
};

/// The consumer-facing name one Kira export is called by.
///
/// Re-exported from the frontend, which derives it once, so a test or a build
/// script never spells the snake_case mapping a second time.
pub use kira_semantics::exported_name as wrapper_export_name;
