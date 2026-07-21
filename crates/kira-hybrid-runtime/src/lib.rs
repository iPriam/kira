//! Hybrid runtime: the host that runs a program whose halves live on two
//! engines.
//!
//! Layer 4 of the Kira package graph.
//!
//! A hybrid build splits one program on its `@Runtime`/`@Native` annotations
//! and emits three artifacts: a bytecode module, a native shared library, and a
//! `.khm` manifest tying them together. This crate loads that bundle and runs
//! it — it is the *middle* of the boundary, and the two ends already exist:
//!
//! - **Runtime to native.** The VM reaches a `@Native` callee and asks its
//!   embedder, in safe Rust, through
//!   [`HostCapabilities::call_native`](kira_runtime_abi::HostCapabilities::call_native).
//!   This crate is that embedder: it resolves the callee's trampoline out of the
//!   loaded library and marshals the call. The VM itself performs no part of it,
//!   which is what keeps its portable core free of `dlopen`.
//! - **Native to runtime.** Generated code calls `kira_hybrid_call_runtime`,
//!   which forwards to an invoker this crate installs at load. So the library
//!   has no undefined symbols and needs no arrangement with whatever loads it.
//!
//! # Native-only, by construction
//!
//! This crate dynamically loads code, so it lives outside the VM's portable
//! cone and never compiles for `wasm32-unknown-unknown`. That is the division of
//! labour the seam is built around: the VM stays portable precisely because the
//! host is not.
//!
//! # Using it
//!
//! ```no_run
//! # fn main() -> Result<(), kira_hybrid_runtime::HybridError> {
//! let session = kira_hybrid_runtime::Session::load(std::path::Path::new("demo.khm"))?;
//! session.run()?;
//! # Ok(())
//! # }
//! ```

pub mod error;
pub mod foreign;
pub mod library;
pub mod marshal;
pub mod session;
pub mod validate;

pub use error::HybridError;
pub use library::NativeLibrary;
pub use marshal::{MarshalError, OwnedArg};
pub use session::Session;
