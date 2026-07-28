//! The embedding surface: how a Rust program loads and calls a Kira library.
//!
//! Layer 5 of the Kira package graph — above `kira-vm-runtime`, which it embeds.
//!
//! # What this crate is for
//!
//! Kira gained an export surface so a library authored in Kira can be *consumed*
//! by a Rust program: the author marks functions `@Export`, `kirac build` emits
//! an artifact plus a generated wrapper crate, and the consumer writes
//! `ui.make_button("ok")`. This crate is what that generated wrapper is built
//! on. It is the whole of the safe machinery — decode a library, check it is the
//! one the wrapper was generated from, instantiate it with a host, call an
//! export by name, release a handle:
//!
//! ```no_run
//! use kira_bytecode::ExportType;
//! use kira_main::{ExportContract, ExpectedExport, Handle, Library};
//! use kira_runtime_abi::{NativeArg, NativeResult};
//!
//! /// What the generator writes into the wrapper crate: the surface it was
//! /// built against, and the hash of the artifact it was built from.
//! const CONTRACT: ExportContract<'static> = ExportContract {
//!     classes: &["Button"],
//!     functions: &[ExpectedExport {
//!         name: "make_button",
//!         params: &[ExportType::String],
//!         result: ExportType::Handle { class: 0 },
//!     }],
//!     content_hash: 0x0123_4567_89ab_cdef,
//! };
//!
//! fn make_a_button(artifact: &[u8]) -> Result<(), kira_main::Error> {
//!     let library = Library::from_bytes(artifact)?;
//!     library.verify(&CONTRACT)?;
//!     let mut ui = library.instantiate()?;
//!     let result = ui.call("make_button", &[NativeArg::Str("ok")])?;
//!     if let NativeResult::Handle(word) = result {
//!         ui.release(Handle::from_word(word))?;
//!     }
//!     Ok(())
//! }
//! ```
//!
//! # Rust first, and deliberately not C
//!
//! The crate's charter has always been "the stable entry points embedders call
//! to load and run Kira programs", and that is what this is — but in Rust, not
//! in C. The reason is asymmetry of cost: **every C signature is append-only
//! forever**, and the only consumer this feature names is Rust. A
//! language-agnostic C facade (`kira_program_load` / `kira_program_call`, for
//! Swift, Zig, or C hosts) is v2 growth of this same crate, and starts when a
//! non-Rust consumer actually exists. Shipping one now would freeze a shape
//! before anything had pulled on it.
//!
//! The packaging follows the same rule as the surface: `rlib` only, because
//! Rust is the only consumer named. Carrying `staticlib` and `cdylib` for the
//! facade that does not exist yet was not free — see the note in `Cargo.toml`
//! for what it cost — and adding them back is one line when it does.
//!
//! # Where the guards are
//!
//! A wrapper and the library it was generated from are built separately, so they
//! can disagree. Both engines guard that, and each uses the only mechanism it
//! has:
//!
//! - **VM engine** — the `.kbc` is data embedded in the wrapper, and there is no
//!   link step to fail, so the check is on data: [`Library::verify`] compares the
//!   export surface and names the first thing that moved.
//! - **Native engine** — the trampolines resolve by name, so the check is a
//!   symbol: the wrapper calls `kira_lib_<library>_abi_1` and a stale archive
//!   fails the consumer's link naming it. See [`abi`].
//!
//! # What this layer does not promise
//!
//! Class typing. A handle is one word here, and [`Instance::call`] checks an
//! argument's *kind* — integer, string, handle — not which class a handle
//! denotes. The generated wrapper mints one Rust newtype per exported class and
//! is where a `Button` stops being passable as a `Window`. Saying so is better
//! than implying a guarantee this layer cannot make.

pub mod abi;
pub mod error;
#[cfg(test)]
mod fixture;
// The native foreign host `dlopen`s a generated sidecar, so it is native-only:
// gated out for wasm exactly as its `kira-dynamic-ffi` dependency is.
#[cfg(not(target_family = "wasm"))]
pub mod callback;
#[cfg(not(target_family = "wasm"))]
pub mod foreign;
pub mod host;
pub mod instance;
pub mod library;

pub use abi::{
    EXPORT_ABI_VERSION, EXPORT_SYMBOL_PREFIX, class_drop_symbol, export_abi_marker, export_symbol,
};
#[cfg(not(target_family = "wasm"))]
pub use callback::ForeignSession;
pub use error::{ContractError, Error, describe_result, describe_tag};
#[cfg(not(target_family = "wasm"))]
pub use foreign::{ForeignBinding, ForeignHost};
pub use host::StdoutHost;
pub use instance::{Handle, Instance};
pub use library::{ExpectedExport, ExportContract, Library, content_hash};
