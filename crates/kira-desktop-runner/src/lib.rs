//! The desktop runner: the client that hosts a Kira app in a live session.
//!
//! This is a runner, not a compiler. It never sees IR, a source file, or a
//! backend — it receives a `.klbundle` over the live protocol and runs what is
//! in it. That is the whole point of the bundle boundary: this binary would keep
//! working if the compiler's insides were replaced tomorrow.
//!
//! It runs both halves of the language, because a Kira app is not VM-only:
//!
//! - a [`PayloadKind::VmBytecode`] entry is decoded and run on the VM,
//! - a [`PayloadKind::HybridManifest`] entry is loaded as a hybrid session — the
//!   bytecode half on the VM, the native half `dlopen`ed and called through.
//!
//! Neither needs LLVM. LLVM builds a bundle; running one is just loading it, so
//! this runner has no LLVM dependency and works the same on a machine without
//! one. That is what lets the native path be a real path here rather than a
//! deferred one.
//!
//! The runner hosts the real Kira Graphics window and event loop for live apps.
//! The bundle owns the UI tree and rendering calls; this binary owns the live
//! session, window, input relay, and frame lifecycle around it.

pub mod host;
pub mod hotpatch;
pub mod relay;
pub mod stage;
pub mod staged;

pub use host::{DesktopHost, DesktopRunnerError};
pub use hotpatch::VmHotPatch;
pub use relay::{AppThread, RelayError, RelayHost};
pub use staged::Staged;
