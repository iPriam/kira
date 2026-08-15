//! The Kira profiler: sampled profiles of a running program, on every backend.
//!
//! Layer 8 of the Kira package graph.
//!
//! # What it is
//!
//! `kira profile` is modelled on `perf`, deliberately and closely. The verbs
//! are `record`, `report`, `annotate`, `script`, `stat`, and `diff`; a
//! recording is one file that every other verb reads; the report's columns are
//! overhead, children, samples, command, shared object, and symbol. Anyone who
//! can read a `perf report` can read this one, and anything driving `perf` can
//! be pointed at this instead.
//!
//! # Two views, one shape
//!
//! Every recording has up to two views of the same run:
//!
//! - the **Kira view**, whose frames are the functions the program's author
//!   wrote, and
//! - the **machine view**, whose frames are what the machine was actually
//!   executing — the interpreter, the runtime, the system, the program's own
//!   machine code.
//!
//! Where each comes from is the only thing that differs between backends, and
//! it is the one thing a reader never has to know. A native run's Kira frames
//! *are* machine frames, recovered from the symbols the LLVM backend emitted. A
//! VM run's Kira frames come from the call stack the interpreter publishes for
//! [`runtime`] to sample, because the machine stack of an interpreter is the
//! interpreter's, not the program's. A hybrid run has both halves and gets both
//! from where they live. The report is the same report in all three cases,
//! which is what makes `--backend vm` and `--backend llvm` comparable at all.
//!
//! # Where the samples come from
//!
//! Machine samples come from the platform's own profiler, never from a sampler
//! Kira reimplemented: `perf` on Linux, Instruments on macOS, the Windows
//! debugging facility on Windows. See [`collect`].

pub mod clock;
pub mod collect;
pub mod counters;
pub mod model;
pub mod render;
pub mod runtime;
pub mod session;
pub mod symbols;
pub mod trace;

pub use collect::{CollectError, CollectOptions, DEFAULT_FREQUENCY, Launch, MachineRecorder};
pub use counters::{InstructionCounter, InstructionProfile};
pub use model::{
    Frame, FrameId, FrameKind, FrameTable, Nanos, Profile, Sample, ThreadId, ThreadRecord, View,
};
pub use runtime::{RuntimeSampler, RuntimeSamples};
pub use session::{ChildSampler, RecordOptions, RecordOutcome, SessionError, record};
pub use symbols::{FunctionIdentity, KiraSymbols, SymbolIdentity};
pub use trace::{Trace, TraceError, TraceMeta};
