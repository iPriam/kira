//! Hosting a `.klbundle` in a process: the runner-side half of a live session.
//!
//! This is a host, not a compiler. It never sees IR, a source file, or a
//! backend — it receives a `.klbundle` (over the live protocol, or read from
//! an application's own resources) and runs what is in it. That is the whole
//! point of the bundle boundary: this crate would keep working if the
//! compiler's insides were replaced tomorrow.
//!
//! Every Kira runner is a thin front over this crate. The desktop runner
//! binary is one front; the embedded application runner that exported Xcode
//! apps carry is another. What they share — everything except how they are
//! launched and which process they run in — lives here:
//!
//! - [`BundleHost`] implements [`RunnerHost`] for a bundle: staging it into a
//!   cache directory, loading its entrypoint (VM bytecode, whole-program
//!   native library, or hybrid session), linking, starting, and swapping.
//! - [`relay`] runs the app on one thread and the protocol on another, which
//!   is what lets a runner host an *app* rather than only a program.
//! - [`VmHotPatch`] swaps VM bytecode in place at a frame boundary.
//!
//! None of it needs LLVM. LLVM builds a bundle; running one is just loading
//! it, so this crate has no LLVM dependency and works the same on a machine
//! without one. That is what lets the native path be a real path here rather
//! than a deferred one.

pub mod host;
pub mod hotpatch;
pub mod native;
pub mod relay;
pub mod stage;
pub mod staged;

pub use host::{BundleHost, BundleHostError};
pub use hotpatch::{VmHotPatch, VmHotPatchStatus};
pub use native::{NativeProgram, NativeProgramError};
pub use relay::{AppThread, RelayError, RelayHost};
pub use staged::Staged;
