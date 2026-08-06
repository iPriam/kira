//! The `KLP1` live protocol: what a server and a runner say to each other.
//!
//! Framing is a length prefix around a tagged body, so a reader always knows how
//! much to read before it knows what it has. Every message is length-delimited
//! and every length is bounded: the peer is a socket, and a socket is not
//! trusted. A hostile or broken peer gets a typed error, never a panic and never
//! an allocation it asked for.
//!
//! The conversation:
//!
//! ```text
//! client -> Hello { protocol, runner }     the runner announces itself
//! server -> Welcome { protocol }           the server accepts, or errors
//! client -> RequestBundle                  the runner asks for the app
//! server -> Manifest { bytes }             the KLB1 manifest, first
//! client -> RequestPayload { name }        then each payload it names
//! server -> Payload { name, bytes }
//! client -> Progress { phase }             milestones, as they actually occur
//! client -> Failed { reason }              or why one did not
//! client -> AppExited { reason }           the app is over; the runner is not
//! client -> Goodbye / server -> Shutdown   either end may end it
//! ```
//!
//! The runner reports its own milestones because only the runner knows them. The
//! server never infers `BundleLoaded` from having sent the bytes.
//!
//! [`frame`] is how a message gets on and off the socket; this module is what
//! the messages *are*.

pub mod frame;

use kira_manifest::RunnerId;

use crate::event::ReloadMode;
use crate::progress::SessionPhase;

pub use frame::{MAX_FRAME_LEN, read_message, write_message};

use frame::{Reader, write_bytes};

/// The magic that opens a `Hello`, identifying the protocol on the wire.
pub const MAGIC: [u8; 4] = *b"KLP1";

/// The protocol version this build speaks.
///
/// Bumped when a message's meaning changes. Appending a new message kind does
/// not bump it: an older peer rejects an unknown tag cleanly, which is the
/// behavior the tag space is append-only for.
pub const PROTOCOL_VERSION: u16 = 1;

/// A message from a runner to the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// The runner announcing itself. Always first.
    Hello {
        /// The protocol version the runner speaks.
        protocol: u16,
        /// Which runner it is.
        runner: RunnerId,
    },
    /// Asking for the app's bundle manifest.
    RequestBundle,
    /// Asking for one payload by name.
    RequestPayload {
        /// The payload's name, as the manifest gave it.
        name: String,
    },
    /// Reporting that a milestone actually occurred.
    Progress {
        /// The milestone reached.
        phase: SessionPhase,
    },
    /// Reporting that the runner could not get further, and why.
    Failed {
        /// What went wrong, in the runner's words.
        reason: String,
    },
    /// The runner is done and closing down cleanly.
    Goodbye,
    /// The runner loaded a rebuilt bundle but has not swapped to it yet.
    ReloadStaged,
    /// The runner swapped to the staged bundle.
    ReloadApplied,
    /// The swapped-in code ran without incident.
    ReloadCompleted,
    /// The runner will not take this hot patch, and why.
    ///
    /// Distinct from [`ClientMessage::RestartRequired`] by whose fault it is: a
    /// rejection is the runner declining an edit its live values cannot survive,
    /// where a restart requirement is the bundle itself being unswappable.
    ReloadRejected {
        /// Why, in the runner's words.
        reason: String,
    },
    /// The runner cannot take this bundle in place at all; it must be relaunched.
    RestartRequired {
        /// Why, in the runner's words.
        reason: String,
    },
    /// The app's entrypoint returned, and the runner is still here.
    ///
    /// Distinct from [`ClientMessage::Goodbye`], which is the *runner* ending.
    /// The two come apart the moment a session hosts something with a run loop:
    /// an app that closes its window has ended while its runner is still up,
    /// holding the cache and the loaded library that make the next reload cheap.
    /// Which of the two ends the session is the supervisor's decision, not the
    /// runner's, so the runner reports the fact and stays.
    AppExited {
        /// Why it stopped, or `None` if it simply finished.
        reason: Option<String>,
    },
}

/// A message from the server to a runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerMessage {
    /// The server accepting a runner's `Hello`.
    Welcome {
        /// The protocol version the server speaks.
        protocol: u16,
    },
    /// The bundle's `KLB1` manifest.
    Manifest {
        /// The encoded manifest.
        bytes: Vec<u8>,
    },
    /// One payload's bytes.
    Payload {
        /// The payload's name.
        name: String,
        /// Its bytes.
        bytes: Vec<u8>,
    },
    /// The runner asked for a payload this bundle does not have.
    NoSuchPayload {
        /// The name it asked for.
        name: String,
    },
    /// The server is ending the session; the runner should shut down cleanly.
    Shutdown,
    /// A rebuilt bundle exists, and the server is asking the runner to take it
    /// in place.
    ///
    /// Carries the new manifest, not the payloads: the runner asks for the
    /// payloads it needs by name, the same way it did for the first bundle. The
    /// mode is always [`ReloadMode::HotPatch`] — a relaunch is not something a
    /// runner does, it is something done to it.
    Reload {
        /// The tier the server is attempting.
        mode: ReloadMode,
        /// The rebuilt bundle's encoded `KLB1` manifest.
        manifest: Vec<u8>,
    },
}

/// An error reading or writing a protocol message.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The socket failed.
    #[error("live protocol i/o failed: {0}")]
    Io(#[from] std::io::Error),
    /// The peer closed the connection between messages.
    #[error("live peer disconnected")]
    Disconnected,
    /// A frame body ended early.
    #[error("truncated live protocol frame")]
    Truncated,
    /// A frame announced a length this build refuses to allocate.
    #[error("live protocol frame of {len} bytes exceeds the {MAX_FRAME_LEN}-byte limit")]
    FrameTooLarge {
        /// The length the peer announced.
        len: u32,
    },
    /// A message tag named no message this build knows.
    #[error("unknown live protocol message tag `{0}`")]
    UnknownTag(u8),
    /// A string in a message was not valid UTF-8.
    #[error("invalid UTF-8 in live protocol message")]
    InvalidString,
    /// A `Hello` did not open with the protocol magic.
    #[error("not a Kira live client (bad magic)")]
    BadMagic,
    /// A runner byte named no runner this build knows.
    #[error("unknown runner `{0}` in live protocol message")]
    UnknownRunner(u8),
    /// A phase byte named no milestone this build knows.
    #[error("unknown session phase `{0}` in live protocol message")]
    UnknownPhase(u8),
    /// A reload-mode byte named no tier this build knows.
    #[error("unknown reload mode `{0}` in live protocol message")]
    UnknownReloadMode(u8),
    /// The peer speaks a version this build does not.
    #[error("live protocol version mismatch: peer speaks {theirs}, this build speaks {ours}")]
    VersionMismatch {
        /// The version the peer announced.
        theirs: u16,
        /// The version this build speaks.
        ours: u16,
    },
}

/// The wire tags for client messages. Append-only: a new message takes the next
/// free byte, so an older peer rejects it as unknown rather than misreading it.
mod client_tag {
    pub const HELLO: u8 = 0;
    pub const REQUEST_BUNDLE: u8 = 1;
    pub const REQUEST_PAYLOAD: u8 = 2;
    pub const PROGRESS: u8 = 3;
    pub const FAILED: u8 = 4;
    pub const GOODBYE: u8 = 5;
    pub const RELOAD_STAGED: u8 = 6;
    pub const RELOAD_APPLIED: u8 = 7;
    pub const RELOAD_COMPLETED: u8 = 8;
    pub const RELOAD_REJECTED: u8 = 9;
    pub const RESTART_REQUIRED: u8 = 10;
    pub const APP_EXITED: u8 = 11;
}

/// The wire tags for server messages; append-only for the same reason.
mod server_tag {
    pub const WELCOME: u8 = 0;
    pub const MANIFEST: u8 = 1;
    pub const PAYLOAD: u8 = 2;
    pub const NO_SUCH_PAYLOAD: u8 = 3;
    pub const SHUTDOWN: u8 = 4;
    pub const RELOAD: u8 = 5;
}

/// A message that can go on the wire.
///
/// A trait rather than one enum of both directions: the two ends send different
/// things, and a server that can construct a `ClientMessage` is a server that
/// can fabricate a runner's milestone report. Keeping the directions distinct
/// makes that a type error.
pub trait Message: Sized {
    /// Encodes the message body (tag included).
    fn encode(&self) -> Vec<u8>;
    /// Decodes a message body (tag included).
    fn decode(body: &[u8]) -> Result<Self, ProtocolError>;
}

impl Message for ClientMessage {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Hello { protocol, runner } => {
                out.push(client_tag::HELLO);
                out.extend_from_slice(&MAGIC);
                out.extend_from_slice(&protocol.to_le_bytes());
                out.push(runner.index() as u8);
            }
            Self::RequestBundle => out.push(client_tag::REQUEST_BUNDLE),
            Self::RequestPayload { name } => {
                out.push(client_tag::REQUEST_PAYLOAD);
                write_bytes(&mut out, name.as_bytes());
            }
            Self::Progress { phase } => {
                out.push(client_tag::PROGRESS);
                out.push(phase.as_byte());
            }
            Self::Failed { reason } => {
                out.push(client_tag::FAILED);
                write_bytes(&mut out, reason.as_bytes());
            }
            Self::Goodbye => out.push(client_tag::GOODBYE),
            Self::ReloadStaged => out.push(client_tag::RELOAD_STAGED),
            Self::ReloadApplied => out.push(client_tag::RELOAD_APPLIED),
            Self::ReloadCompleted => out.push(client_tag::RELOAD_COMPLETED),
            Self::ReloadRejected { reason } => {
                out.push(client_tag::RELOAD_REJECTED);
                write_bytes(&mut out, reason.as_bytes());
            }
            Self::RestartRequired { reason } => {
                out.push(client_tag::RESTART_REQUIRED);
                write_bytes(&mut out, reason.as_bytes());
            }
            Self::AppExited { reason } => {
                out.push(client_tag::APP_EXITED);
                // A flag rather than an empty string: an app that failed with
                // nothing to say and one that finished cleanly are different
                // outcomes, and a session reports which.
                out.push(u8::from(reason.is_some()));
                write_bytes(&mut out, reason.as_deref().unwrap_or_default().as_bytes());
            }
        }
        out
    }

    fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut reader = Reader::new(body);
        let tag = reader.take(1)?[0];
        match tag {
            client_tag::HELLO => {
                if reader.take(4)? != MAGIC {
                    return Err(ProtocolError::BadMagic);
                }
                let protocol = reader.read_u16()?;
                let raw = reader.take(1)?[0];
                let runner = RunnerId::all()
                    .get(raw as usize)
                    .copied()
                    .ok_or(ProtocolError::UnknownRunner(raw))?;
                Ok(Self::Hello { protocol, runner })
            }
            client_tag::REQUEST_BUNDLE => Ok(Self::RequestBundle),
            client_tag::REQUEST_PAYLOAD => Ok(Self::RequestPayload {
                name: reader.read_string()?,
            }),
            client_tag::PROGRESS => {
                let raw = reader.take(1)?[0];
                Ok(Self::Progress {
                    phase: SessionPhase::from_byte(raw).ok_or(ProtocolError::UnknownPhase(raw))?,
                })
            }
            client_tag::FAILED => Ok(Self::Failed {
                reason: reader.read_string()?,
            }),
            client_tag::GOODBYE => Ok(Self::Goodbye),
            client_tag::RELOAD_STAGED => Ok(Self::ReloadStaged),
            client_tag::RELOAD_APPLIED => Ok(Self::ReloadApplied),
            client_tag::RELOAD_COMPLETED => Ok(Self::ReloadCompleted),
            client_tag::RELOAD_REJECTED => Ok(Self::ReloadRejected {
                reason: reader.read_string()?,
            }),
            client_tag::RESTART_REQUIRED => Ok(Self::RestartRequired {
                reason: reader.read_string()?,
            }),
            client_tag::APP_EXITED => {
                let failed = reader.take(1)?[0] != 0;
                let reason = reader.read_string()?;
                Ok(Self::AppExited {
                    reason: failed.then_some(reason),
                })
            }
            other => Err(ProtocolError::UnknownTag(other)),
        }
    }
}

impl Message for ServerMessage {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            Self::Welcome { protocol } => {
                out.push(server_tag::WELCOME);
                out.extend_from_slice(&protocol.to_le_bytes());
            }
            Self::Manifest { bytes } => {
                out.push(server_tag::MANIFEST);
                write_bytes(&mut out, bytes);
            }
            Self::Payload { name, bytes } => {
                out.push(server_tag::PAYLOAD);
                write_bytes(&mut out, name.as_bytes());
                write_bytes(&mut out, bytes);
            }
            Self::NoSuchPayload { name } => {
                out.push(server_tag::NO_SUCH_PAYLOAD);
                write_bytes(&mut out, name.as_bytes());
            }
            Self::Shutdown => out.push(server_tag::SHUTDOWN),
            Self::Reload { mode, manifest } => {
                out.push(server_tag::RELOAD);
                out.push(mode.as_byte());
                write_bytes(&mut out, manifest);
            }
        }
        out
    }

    fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut reader = Reader::new(body);
        let tag = reader.take(1)?[0];
        match tag {
            server_tag::WELCOME => Ok(Self::Welcome {
                protocol: reader.read_u16()?,
            }),
            server_tag::MANIFEST => Ok(Self::Manifest {
                bytes: reader.read_len_prefixed()?.to_vec(),
            }),
            server_tag::PAYLOAD => {
                let name = reader.read_string()?;
                let bytes = reader.read_len_prefixed()?.to_vec();
                Ok(Self::Payload { name, bytes })
            }
            server_tag::NO_SUCH_PAYLOAD => Ok(Self::NoSuchPayload {
                name: reader.read_string()?,
            }),
            server_tag::SHUTDOWN => Ok(Self::Shutdown),
            server_tag::RELOAD => {
                let raw = reader.take(1)?[0];
                let mode =
                    ReloadMode::from_byte(raw).ok_or(ProtocolError::UnknownReloadMode(raw))?;
                Ok(Self::Reload {
                    mode,
                    manifest: reader.read_len_prefixed()?.to_vec(),
                })
            }
            other => Err(ProtocolError::UnknownTag(other)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client_messages() -> Vec<ClientMessage> {
        let mut messages = vec![
            ClientMessage::RequestBundle,
            ClientMessage::RequestPayload {
                name: "app.kbc".to_owned(),
            },
            ClientMessage::Failed {
                reason: "dlopen failed".to_owned(),
            },
            ClientMessage::Goodbye,
            ClientMessage::ReloadStaged,
            ClientMessage::ReloadApplied,
            ClientMessage::ReloadCompleted,
            ClientMessage::ReloadRejected {
                reason: "a live closure lost its function".to_owned(),
            },
            ClientMessage::RestartRequired {
                reason: "the native library changed".to_owned(),
            },
            // Both outcomes: an app that failed with nothing to say and one
            // that simply finished must not decode as each other.
            ClientMessage::AppExited { reason: None },
            ClientMessage::AppExited {
                reason: Some(String::new()),
            },
            ClientMessage::AppExited {
                reason: Some("vm: divide by zero".to_owned()),
            },
        ];
        for runner in RunnerId::all() {
            messages.push(ClientMessage::Hello {
                protocol: PROTOCOL_VERSION,
                runner,
            });
        }
        for phase in [
            SessionPhase::Connected,
            SessionPhase::BundleSent,
            SessionPhase::BundleReceived,
            SessionPhase::BundleLoaded,
            SessionPhase::BundleLinked,
            SessionPhase::EntrypointStarted,
            SessionPhase::FramePresented,
        ] {
            messages.push(ClientMessage::Progress { phase });
        }
        messages
    }

    fn server_messages() -> Vec<ServerMessage> {
        vec![
            ServerMessage::Welcome {
                protocol: PROTOCOL_VERSION,
            },
            ServerMessage::Manifest {
                bytes: b"KLB1 manifest".to_vec(),
            },
            ServerMessage::Payload {
                name: "app.kbc".to_owned(),
                bytes: b"KBC1 code".to_vec(),
            },
            ServerMessage::NoSuchPayload {
                name: "absent".to_owned(),
            },
            ServerMessage::Shutdown,
            ServerMessage::Reload {
                mode: ReloadMode::HotPatch,
                manifest: b"KLB1 rebuilt".to_vec(),
            },
            ServerMessage::Reload {
                mode: ReloadMode::Relaunch,
                manifest: b"KLB1 rebuilt".to_vec(),
            },
        ]
    }

    #[test]
    fn every_client_message_round_trips_through_a_frame() {
        for message in client_messages() {
            let mut wire = Vec::new();
            write_message(&mut wire, &message).expect("write");
            let decoded: ClientMessage =
                read_message(&mut wire.as_slice()).expect("read back what was written");
            assert_eq!(decoded, message);
        }
    }

    #[test]
    fn every_server_message_round_trips_through_a_frame() {
        for message in server_messages() {
            let mut wire = Vec::new();
            write_message(&mut wire, &message).expect("write");
            let decoded: ServerMessage =
                read_message(&mut wire.as_slice()).expect("read back what was written");
            assert_eq!(decoded, message);
        }
    }

    /// Frames are self-delimiting: a stream of them reads back one at a time,
    /// with no framing bugs that only show up under batching.
    #[test]
    fn frames_stream_back_to_back() {
        let mut wire = Vec::new();
        for message in server_messages() {
            write_message(&mut wire, &message).expect("write");
        }
        let mut cursor = wire.as_slice();
        for expected in server_messages() {
            let decoded: ServerMessage = read_message(&mut cursor).expect("read");
            assert_eq!(decoded, expected);
        }
        assert!(matches!(
            read_message::<_, ServerMessage>(&mut cursor),
            Err(ProtocolError::Disconnected)
        ));
    }

    /// Every truncation of every message is an error and none of them panic.
    #[test]
    fn every_truncated_frame_is_rejected() {
        for message in client_messages() {
            let mut wire = Vec::new();
            write_message(&mut wire, &message).expect("write");
            for len in 0..wire.len() {
                let result = read_message::<_, ClientMessage>(&mut &wire[..len]);
                assert!(
                    result.is_err(),
                    "a {len}-byte prefix of {message:?} must not decode"
                );
            }
        }
    }

    #[test]
    fn an_empty_stream_reads_as_disconnected() {
        assert!(matches!(
            read_message::<_, ClientMessage>(&mut b"".as_slice()),
            Err(ProtocolError::Disconnected)
        ));
    }

    /// The frame limit is checked against the announced length before anything
    /// is allocated, so a peer cannot ask this process to reserve gigabytes.
    #[test]
    fn an_oversized_frame_is_refused_before_allocating() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&(MAX_FRAME_LEN + 1).to_le_bytes());
        // Deliberately no body: if the limit were checked after reading, this
        // would block or fail on the read instead of reporting the limit.
        let error = read_message::<_, ClientMessage>(&mut wire.as_slice())
            .expect_err("an oversized frame must be refused");
        assert!(
            matches!(
                error,
                ProtocolError::FrameTooLarge {
                    len
                } if len == MAX_FRAME_LEN + 1
            ),
            "expected a frame-too-large error, got {error:?}"
        );
    }

    #[test]
    fn an_unknown_client_tag_is_rejected() {
        let error = ClientMessage::decode(&[200]).expect_err("unknown tag");
        assert!(matches!(error, ProtocolError::UnknownTag(200)));
    }

    #[test]
    fn an_unknown_server_tag_is_rejected() {
        let error = ServerMessage::decode(&[200]).expect_err("unknown tag");
        assert!(matches!(error, ProtocolError::UnknownTag(200)));
    }

    #[test]
    fn an_empty_body_is_rejected() {
        assert!(matches!(
            ClientMessage::decode(&[]),
            Err(ProtocolError::Truncated)
        ));
    }

    /// A `Hello` that is not a Kira runner is refused on the magic, so a stray
    /// connection from something else fails clearly rather than as a tag error.
    #[test]
    fn a_hello_without_the_magic_is_rejected() {
        let mut body = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            runner: RunnerId::Desktop,
        }
        .encode();
        body[1] = b'X';
        assert!(matches!(
            ClientMessage::decode(&body),
            Err(ProtocolError::BadMagic)
        ));
    }

    #[test]
    fn an_unknown_runner_in_hello_is_rejected() {
        let mut body = ClientMessage::Hello {
            protocol: PROTOCOL_VERSION,
            runner: RunnerId::Desktop,
        }
        .encode();
        let runner_at = body.len() - 1;
        body[runner_at] = 99;
        assert!(matches!(
            ClientMessage::decode(&body),
            Err(ProtocolError::UnknownRunner(99))
        ));
    }

    #[test]
    fn an_unknown_phase_is_rejected() {
        let body = [client_tag::PROGRESS, 99];
        assert!(matches!(
            ClientMessage::decode(&body),
            Err(ProtocolError::UnknownPhase(99))
        ));
    }

    #[test]
    fn invalid_utf8_in_a_message_is_rejected() {
        let mut body = vec![client_tag::REQUEST_PAYLOAD];
        write_bytes(&mut body, &[0xff, 0xfe]);
        assert!(matches!(
            ClientMessage::decode(&body),
            Err(ProtocolError::InvalidString)
        ));
    }

    /// A length prefix inside a body that overruns it is truncation, not a panic.
    #[test]
    fn an_overrunning_inner_length_is_rejected() {
        let mut body = vec![client_tag::REQUEST_PAYLOAD];
        body.extend_from_slice(&u32::MAX.to_le_bytes());
        body.extend_from_slice(b"short");
        assert!(matches!(
            ClientMessage::decode(&body),
            Err(ProtocolError::Truncated)
        ));
    }

    /// The message tags are equally a contract; a renumber breaks every runner.
    #[test]
    fn message_tags_are_pinned() {
        assert_eq!(client_tag::HELLO, 0);
        assert_eq!(client_tag::REQUEST_BUNDLE, 1);
        assert_eq!(client_tag::REQUEST_PAYLOAD, 2);
        assert_eq!(client_tag::PROGRESS, 3);
        assert_eq!(client_tag::FAILED, 4);
        assert_eq!(client_tag::GOODBYE, 5);
        assert_eq!(client_tag::RELOAD_STAGED, 6);
        assert_eq!(client_tag::RELOAD_APPLIED, 7);
        assert_eq!(client_tag::RELOAD_COMPLETED, 8);
        assert_eq!(client_tag::RELOAD_REJECTED, 9);
        assert_eq!(client_tag::RESTART_REQUIRED, 10);
        assert_eq!(server_tag::WELCOME, 0);
        assert_eq!(server_tag::MANIFEST, 1);
        assert_eq!(server_tag::PAYLOAD, 2);
        assert_eq!(server_tag::NO_SUCH_PAYLOAD, 3);
        assert_eq!(server_tag::SHUTDOWN, 4);
        assert_eq!(server_tag::RELOAD, 5);
    }

    /// The reload tiers are a wire contract too: a runner from another checkout
    /// reads these bytes to know which tier it is being asked for, and reading
    /// the wrong one means swapping code into a process that cannot take it.
    #[test]
    fn reload_mode_wire_bytes_are_pinned() {
        assert_eq!(ReloadMode::HotPatch.as_byte(), 0);
        assert_eq!(ReloadMode::Relaunch.as_byte(), 1);
        assert_eq!(ReloadMode::from_byte(0), Some(ReloadMode::HotPatch));
        assert_eq!(ReloadMode::from_byte(1), Some(ReloadMode::Relaunch));
        assert_eq!(ReloadMode::from_byte(2), None);
    }

    #[test]
    fn an_unknown_reload_mode_is_rejected() {
        let body = [server_tag::RELOAD, 99, 0, 0, 0, 0];
        assert!(matches!(
            ServerMessage::decode(&body),
            Err(ProtocolError::UnknownReloadMode(99))
        ));
    }
}
