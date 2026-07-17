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
//! The runner is deliberately headless. Presenting a frame means a window and a
//! swapchain, which this repo does not own — kira-graphics does. A headless
//! session is honest about stopping at the entrypoint rather than claiming a
//! frame it never drew.

pub mod host;

pub use host::{DesktopHost, DesktopRunnerError};
