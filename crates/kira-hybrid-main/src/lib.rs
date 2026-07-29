//! The hybrid engine's embedding surface: a Kira library whose halves live on
//! two engines, consumed from Rust.
//!
//! Layer 6 of the Kira package graph — above `kira-main` (5), which it embeds
//! for the bytecode half, and above `kira-hybrid-runtime` (4), which it embeds
//! for the native half.
//!
//! # The third engine, and what it is for
//!
//! A Kira library can be built three ways, and all three are fronted by the same
//! generated Rust API — that is where parity is measured, and the engine is the
//! axis underneath it:
//!
//! | Engine | Artifact a consumer gets | Every function runs as |
//! |---|---|---|
//! | VM | a `.kbc` embedded in the crate | bytecode |
//! | native | a static archive the crate links | machine code |
//! | **hybrid** | a `.kbc` + a `.khm`, both embedded, plus a shared library | **whichever the author annotated** |
//!
//! The third row is the reason this engine exists rather than being a parity
//! checkbox. The other two ignore `@Runtime`/`@Native` — the VM engine compiles
//! everything to bytecode, the native engine compiles everything to machine code
//! — and neither is wrong to. The hybrid engine is the only one that *honors*
//! the annotation, so it is the only one where a library author can put a hot
//! inner loop in machine code and keep the surface, the handles, and the strings
//! on the VM.
//!
//! # What it costs, stated rather than hidden
//!
//! - **`libloading` enters the consumer's dependency graph.** The native half is
//!   `dlopen`ed, so the consumer links a dynamic loader. The VM engine needs no
//!   such thing, which is why it stays the default.
//! - **It does not build for `wasm32-unknown-unknown`**, and cannot: this crate
//!   sits above `kira-hybrid-runtime`, which is outside the VM's portable cone
//!   by construction. A Rust wasm application that embeds a Kira library builds
//!   against the **VM engine**, which is this feature's wasm answer and is
//!   unaffected.
//! - **One file has to still be there at run time.** The deployment story is
//!   [`locate`]'s whole subject, and it is a designed search order with a
//!   typed failure that names every path tried.
//!
//! # Using it
//!
//! Nothing writes this by hand — `kira build --backend hybrid` generates a
//! wrapper crate that does — but this is what the generated code does:
//!
//! ```no_run
//! use kira_hybrid_main::HybridLibrary;
//! use kira_runtime_abi::NativeArg;
//!
//! // In the generated crate these two are `include_bytes!` of the `.kbc` and
//! // the `.khm` sitting at its root; only the third artifact stays a file.
//! fn make_a_button(
//!     bytecode: &[u8],
//!     manifest: &[u8],
//!     native_half: &std::path::Path,
//! ) -> Result<(), kira_hybrid_main::HybridMainError> {
//!     let library = HybridLibrary::from_parts("uifoundation", bytecode, manifest)?;
//!     let mut ui = library.instantiate(native_half)?;
//!     let button = ui.call("make_button", &[NativeArg::Str("ok")])?;
//!     let _ = button;
//!     Ok(())
//! }
//! ```

pub mod error;
pub mod instance;
pub mod library;
pub mod locate;

pub use error::HybridMainError;
pub use instance::HybridInstance;
pub use library::HybridLibrary;
pub use locate::{override_variable, shared_library_file_name};

// Re-exported rather than restated, so a generated wrapper names one crate for
// its whole surface. These are `kira-main`'s types unchanged: the export
// contract a wrapper checks itself against, and the handle a consumer holds, are
// the *bytecode half's*, and the bytecode half is a VM-engine library. Aliasing
// them here is what keeps the two generated wrappers' imports the same shape.
pub use kira_main::{ExpectedExport, ExportContract, Handle, StdoutHost};
