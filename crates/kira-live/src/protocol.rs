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
//! client -> Goodbye / server -> Shutdown   either end may end it
//! ```
//!
//! The runner reports its own milestones because only the runner knows them. The
//! server never infers `BundleLoaded` from having sent the bytes.

use std::io::{Read, Write};

use kira_manifest::RunnerId;

use crate::event::SessionPhase;

/// The magic that opens a `Hello`, identifying the protocol on the wire.
pub const MAGIC: [u8; 4] = *b"KLP1";

/// The protocol version this build speaks.
///
/// Bumped when a message's meaning changes. Appending a new message kind does
/// not bump it: an older peer rejects an unknown tag cleanly, which is the
/// behavior the tag space is append-only for.
pub const PROTOCOL_VERSION: u16 = 1;

/// The largest frame body this build will read, in bytes.
///
/// A frame carries one payload, and a native library is genuinely large, so this
/// is generous. It exists because the length prefix is attacker-controlled: it
/// bounds what a peer can make this process try to allocate.
pub const MAX_FRAME_LEN: u32 = 512 * 1024 * 1024;

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
}

/// The wire tags for server messages; append-only for the same reason.
mod server_tag {
    pub const WELCOME: u8 = 0;
    pub const MANIFEST: u8 = 1;
    pub const PAYLOAD: u8 = 2;
    pub const NO_SUCH_PAYLOAD: u8 = 3;
    pub const SHUTDOWN: u8 = 4;
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
                out.push(phase_byte(*phase));
            }
            Self::Failed { reason } => {
                out.push(client_tag::FAILED);
                write_bytes(&mut out, reason.as_bytes());
            }
            Self::Goodbye => out.push(client_tag::GOODBYE),
        }
        out
    }

    fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut reader = Reader {
            bytes: body,
            offset: 0,
        };
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
                    phase: phase_from_byte(raw).ok_or(ProtocolError::UnknownPhase(raw))?,
                })
            }
            client_tag::FAILED => Ok(Self::Failed {
                reason: reader.read_string()?,
            }),
            client_tag::GOODBYE => Ok(Self::Goodbye),
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
        }
        out
    }

    fn decode(body: &[u8]) -> Result<Self, ProtocolError> {
        let mut reader = Reader {
            bytes: body,
            offset: 0,
        };
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
            other => Err(ProtocolError::UnknownTag(other)),
        }
    }
}

/// Writes one message as a length-prefixed frame and flushes it.
///
/// Flushed here rather than left to the caller: every message in this protocol
/// is something the peer is already blocked waiting for, so a buffered write is
/// a deadlock.
pub fn write_message<W: Write, M: Message>(
    writer: &mut W,
    message: &M,
) -> Result<(), ProtocolError> {
    let body = message.encode();
    let len =
        u32::try_from(body.len()).map_err(|_| ProtocolError::FrameTooLarge { len: u32::MAX })?;
    if len > MAX_FRAME_LEN {
        return Err(ProtocolError::FrameTooLarge { len });
    }
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

/// Reads one length-prefixed frame and decodes the message in it.
pub fn read_message<R: Read, M: Message>(reader: &mut R) -> Result<M, ProtocolError> {
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes) {
        Ok(()) => {}
        // A clean close between frames is the peer leaving, not a broken stream.
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
            return Err(ProtocolError::Disconnected);
        }
        Err(error) => return Err(ProtocolError::Io(error)),
    }
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME_LEN {
        // Refused before allocating: the length is the peer's claim, not a fact.
        return Err(ProtocolError::FrameTooLarge { len });
    }
    let mut body = vec![0u8; len as usize];
    reader
        .read_exact(&mut body)
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => ProtocolError::Truncated,
            _ => ProtocolError::Io(error),
        })?;
    M::decode(&body)
}

/// The wire byte for a session phase.
///
/// Append-only, and pinned by a test: a runner built from an older checkout
/// reports milestones with these bytes.
fn phase_byte(phase: SessionPhase) -> u8 {
    match phase {
        SessionPhase::Connected => 0,
        SessionPhase::BundleSent => 1,
        SessionPhase::BundleReceived => 2,
        SessionPhase::BundleLoaded => 3,
        SessionPhase::BundleLinked => 4,
        SessionPhase::EntrypointStarted => 5,
        SessionPhase::FramePresented => 6,
    }
}

/// The phase a wire byte names, or `None` if this build knows no such phase.
fn phase_from_byte(byte: u8) -> Option<SessionPhase> {
    match byte {
        0 => Some(SessionPhase::Connected),
        1 => Some(SessionPhase::BundleSent),
        2 => Some(SessionPhase::BundleReceived),
        3 => Some(SessionPhase::BundleLoaded),
        4 => Some(SessionPhase::BundleLinked),
        5 => Some(SessionPhase::EntrypointStarted),
        6 => Some(SessionPhase::FramePresented),
        _ => None,
    }
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// A bounds-checked cursor over a frame body.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        let end = self.offset.checked_add(n).ok_or(ProtocolError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProtocolError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_len_prefixed(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    fn read_string(&mut self) -> Result<String, ProtocolError> {
        let bytes = self.read_len_prefixed()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ProtocolError::InvalidString)
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

    /// The phase bytes are the wire contract with runners built from other
    /// checkouts, so they are pinned literally rather than left to the match arm.
    #[test]
    fn phase_wire_bytes_are_pinned() {
        let expected = [
            (SessionPhase::Connected, 0u8),
            (SessionPhase::BundleSent, 1),
            (SessionPhase::BundleReceived, 2),
            (SessionPhase::BundleLoaded, 3),
            (SessionPhase::BundleLinked, 4),
            (SessionPhase::EntrypointStarted, 5),
            (SessionPhase::FramePresented, 6),
        ];
        for (phase, byte) in expected {
            assert_eq!(phase_byte(phase), byte, "wire byte for {phase:?}");
            assert_eq!(phase_from_byte(byte), Some(phase));
        }
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
        assert_eq!(server_tag::WELCOME, 0);
        assert_eq!(server_tag::MANIFEST, 1);
        assert_eq!(server_tag::PAYLOAD, 2);
        assert_eq!(server_tag::NO_SUCH_PAYLOAD, 3);
        assert_eq!(server_tag::SHUTDOWN, 4);
    }
}
