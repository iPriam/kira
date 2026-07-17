//! Live/hot-reload platform: runners, live sessions, and module swap.
//!
//! Layer 8 of the Kira package graph.
//!
//! Kira Live is a server/client system, not a rebuild-and-restart loop. A live
//! server builds an app into a [`Bundle`] and serves it over a socket; a runner
//! client connects, downloads it, loads it, links it, and starts its
//! entrypoint, reporting each milestone back as a [`LiveEvent`].
//!
//! Reload is not built yet: a source change today means running the session
//! again. Some of what it will need is already here — payloads named by content
//! hash, and a [`PayloadKind::is_hot_swappable`] that says which of them a
//! running process could take in place — but nothing watches, rebuilds, or
//! swaps, and the `live.reload.*` events are modeled rather than emitted.
//!
//! The pieces, bottom up:
//!
//! - [`hash`] — the content hash every payload is named by,
//! - [`bundle`] — the `KLB1` manifest: the artifact boundary a runner consumes,
//! - [`store`] — a `.klbundle` directory on disk, verified on read,
//! - [`protocol`] — the `KLP1` framing and messages the two ends speak,
//! - [`event`] — the session's observable vocabulary,
//! - [`server`] — the live server: serves bundles, tracks a client,
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
pub mod server;
pub mod store;

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
pub use server::{LiveServer, ServerError};
pub use store::{Bundle, BundleError, NamedPayload};
