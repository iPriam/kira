//! Live/hot-reload platform: runners, live sessions, and module swap.
//!
//! Layer 8 of the Kira package graph.
//!
//! Kira Live is a server/client system, not a rebuild-and-restart loop. A live
//! server builds an app into a [`Bundle`] and serves it over a socket; a runner
//! client connects, downloads it, loads it, links it, and starts its
//! entrypoint, reporting each milestone back as a [`LiveEvent`].
//!
//! A later source change rebuilds the bundle, and [`reload`] decides how it
//! reaches the app: swapped into the running process when the rebuilt native
//! library is byte-for-byte the loaded one, and by replacing the runner when it
//! is not. The decision is never silent — a bundle that cannot be swapped says
//! why.
//!
//! The pieces, bottom up:
//!
//! - [`hash`] — the content hash every payload is named by,
//! - [`bundle`] — the `KLB1` manifest: the artifact boundary a runner consumes,
//! - [`store`] — a `.klbundle` directory on disk, verified on read,
//! - [`protocol`] — the `KLP1` framing and messages the two ends speak,
//! - [`event`] — the session's observable vocabulary,
//! - [`watch`] — a program's inputs, and what is deliberately not watched,
//! - [`reload`] — which tier a rebuilt bundle deserves, and why,
//! - [`server`] — the live server: binds the port, accepts runners,
//! - [`session`] — one runner's session, over its life, reloads included,
//! - [`client`] — the runner client's half of the protocol.
//!
//! **The bundle is the boundary.** A runner reads a bundle and nothing else —
//! never a compiler data structure, never a path into a build directory. That is
//! what lets the compiler's internals change without breaking every runner, and
//! it is why the bundle format is a validated wire format rather than a struct
//! that happens to be serializable.
//!
//! **Only real milestones emit events.** A [`LiveEvent`] is emitted where the
//! thing it names actually happened — `EntrypointStarted` after the entrypoint
//! returns control, not after the bundle was sent in the hope that it will. A
//! session that cannot reach a milestone reports why instead of reporting the
//! milestone.

pub mod bundle;
pub mod client;
pub mod event;
pub mod hash;
pub mod protocol;
pub mod reload;
pub mod server;
pub mod session;
pub mod store;
pub mod watch;

pub use bundle::{
    BundleDecodeError, BundleManifest, MANIFEST_FILE, PAYLOAD_DIR, PayloadEntry, PayloadKind,
};
pub use client::{ClientError, RunnerClient, RunnerHost};
pub use event::{LiveEvent, ProgressError, ReloadMode, SessionPhase, SessionProgress};
pub use hash::{ContentHash, HASH_LEN};
pub use protocol::{
    ClientMessage, Message, PROTOCOL_VERSION, ProtocolError, ServerMessage, read_message,
    write_message,
};
pub use reload::{RelaunchReason, ReloadDecision, decide, hotpatch_disabled_by_env};
pub use server::{LiveServer, ServerError};
pub use session::{LiveSession, ReloadOutcome};
pub use store::{Bundle, BundleError, NamedPayload};
pub use watch::{Change, ChangeKind, SourceWatcher, WatchSet};
