//! Framing: getting one message on and off a socket intact.
//!
//! A length prefix around a tagged body, so a reader always knows how much to
//! read before it knows what it has. That is the whole format, and the rest of
//! this module is the consequence of one fact: the peer is a socket, and a socket
//! is not trusted.
//!
//! So the length is bounded *before* anything is allocated — an attacker-chosen
//! prefix must not be a request this process honors — and every read inside a
//! body is bounds-checked, so a malformed frame is a typed error and never a
//! panic.

use std::io::{Read, Write};
use std::time::Duration;

use super::{Message, ProtocolError};

/// The largest frame body this build will read, in bytes.
///
/// A frame carries one payload, and a native library is genuinely large, so this
/// is generous. It exists because the length prefix is attacker-controlled: it
/// bounds what a peer can make this process try to allocate.
pub const MAX_FRAME_LEN: u32 = 512 * 1024 * 1024;

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
    // A write to a peer that already left is that peer having left, not a
    // broken stream — the same event `read_message` reports, arriving from the
    // other direction because nothing had read since it went.
    write_all_retrying(writer, &len.to_le_bytes())?;
    write_all_retrying(writer, &body)?;
    sent(writer.flush())?;
    Ok(())
}

/// Writes every byte, waiting out transport congestion rather than dying of it.
///
/// macOS answers an over-large or crowded loopback write with `ENOBUFS`, some-
/// times immediately rather than after the socket's send timeout, where Linux
/// would queue the bytes or block; a non-blocking socket answers `WouldBlock`.
/// Treating either as fatal killed whole sessions over congestion one more
/// attempt clears, so those outcomes park briefly and try again — writing from
/// where the last attempt stopped, since a large frame rarely goes out in one
/// piece. Every attempt still runs under the socket's send timeout, so a peer
/// that genuinely stopped reading stays bounded by it. Any other error, and an
/// error that survives the retries, is mapped exactly as a single write was.
fn write_all_retrying<W: Write>(writer: &mut W, mut bytes: &[u8]) -> Result<(), ProtocolError> {
    let mut pauses = 0;
    while !bytes.is_empty() {
        match writer.write(bytes) {
            Ok(0) => return Err(ProtocolError::Io(std::io::ErrorKind::WriteZero.into())),
            Ok(written) => bytes = &bytes[written..],
            Err(error) if congested(&error) && pauses < WRITE_RETRY_LIMIT => {
                pauses += 1;
                std::thread::sleep(WRITE_RETRY_PAUSE);
            }
            Err(error) => return sent(Err(error)),
        }
    }
    Ok(())
}

/// How many times a congested write may park and try again.
///
/// Pauses are short and writes between them are not free — each blocked
/// attempt already spent up to the socket's send timeout inside the kernel —
/// so this bounds the pathological case without making any real transfer think
/// about it.
const WRITE_RETRY_LIMIT: u32 = 200;

/// How long one retry parks before trying again.
const WRITE_RETRY_PAUSE: Duration = Duration::from_millis(25);

/// Whether this write error is transport congestion rather than bad news.
fn congested(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    // ENOBUFS: Darwin spells it 55, Linux 105. Matched by number because the
    // kind both platforms report is `Other`, which would match everything.
    match error.raw_os_error() {
        Some(errno) if cfg!(target_os = "macos") => errno == 55,
        Some(errno) if cfg!(target_os = "linux") => errno == 105,
        _ => false,
    }
}

/// Maps a write's outcome, treating a vanished peer as a disconnect.
fn sent(outcome: std::io::Result<()>) -> Result<(), ProtocolError> {
    match outcome {
        Ok(()) => Ok(()),
        Err(error) if peer_left(&error) => Err(ProtocolError::Disconnected),
        Err(error) => Err(ProtocolError::Io(error)),
    }
}

/// Reads one length-prefixed frame and decodes the message in it.
pub fn read_message<R: Read, M: Message>(reader: &mut R) -> Result<M, ProtocolError> {
    let mut len_bytes = [0u8; 4];
    match reader.read_exact(&mut len_bytes) {
        Ok(()) => {}
        // A close between frames is the peer leaving, not a broken stream —
        // whether or not it was a polite one.
        Err(error) if peer_left(&error) => return Err(ProtocolError::Disconnected),
        Err(error) => return Err(ProtocolError::Io(error)),
    }
    let len = u32::from_le_bytes(len_bytes);
    if len > MAX_FRAME_LEN {
        // Refused before allocating: the length is the peer's claim, not a fact.
        return Err(ProtocolError::FrameTooLarge { len });
    }
    let mut body = vec![0u8; len as usize];
    reader.read_exact(&mut body).map_err(|error| {
        // Mid-frame, a peer that left took the rest of the message with it,
        // which is a truncated frame however the stream ended.
        if peer_left(&error) {
            ProtocolError::Truncated
        } else {
            ProtocolError::Io(error)
        }
    })?;
    M::decode(&body)
}

/// Whether an error means the peer is gone rather than the stream is broken.
///
/// A host that exits closes its end, and what that looks like here depends on
/// the platform: a graceful shutdown reads as end-of-file, and an abrupt one —
/// which is what a process simply exiting produces on Windows — arrives as
/// `ConnectionAborted` or `ConnectionReset`. Both are the same event, and
/// treating only the first as leaving made a session that ran to completion
/// report an i/o failure on one platform and success on the other.
fn peer_left(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::BrokenPipe
    )
}

/// Appends a length-prefixed byte string.
pub(super) fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    out.extend_from_slice(bytes);
}

/// A bounds-checked cursor over a frame body.
pub(super) struct Reader<'a> {
    pub(super) bytes: &'a [u8],
    pub(super) offset: usize,
}

impl<'a> Reader<'a> {
    /// A cursor over `bytes`.
    pub(super) fn new(bytes: &'a [u8]) -> Reader<'a> {
        Reader { bytes, offset: 0 }
    }

    pub(super) fn take(&mut self, n: usize) -> Result<&'a [u8], ProtocolError> {
        // Checked rather than `self.offset + n`: a hostile length would overflow
        // and wrap to a small end that slices successfully.
        let end = self.offset.checked_add(n).ok_or(ProtocolError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(ProtocolError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    pub(super) fn read_u16(&mut self) -> Result<u16, ProtocolError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(super) fn read_u32(&mut self) -> Result<u32, ProtocolError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(super) fn read_len_prefixed(&mut self) -> Result<&'a [u8], ProtocolError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    pub(super) fn read_string(&mut self) -> Result<String, ProtocolError> {
        let bytes = self.read_len_prefixed()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| ProtocolError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientMessage;

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
            matches!(error, ProtocolError::FrameTooLarge { len } if len == MAX_FRAME_LEN + 1),
            "expected a frame-too-large error, got {error:?}"
        );
    }

    #[test]
    fn an_empty_stream_reads_as_disconnected() {
        assert!(matches!(
            read_message::<_, ClientMessage>(&mut b"".as_slice()),
            Err(ProtocolError::Disconnected)
        ));
    }

    /// A body that ends mid-frame is truncation, not a disconnect: the peer said
    /// how much was coming and then did not send it.
    #[test]
    fn a_body_that_ends_early_is_truncated() {
        let mut wire = Vec::new();
        wire.extend_from_slice(&64u32.to_le_bytes());
        wire.extend_from_slice(b"short");
        assert!(matches!(
            read_message::<_, ClientMessage>(&mut wire.as_slice()),
            Err(ProtocolError::Truncated)
        ));
    }

    /// A cursor never reads past its slice, whatever it is asked for.
    #[test]
    fn a_reader_never_runs_past_its_end() {
        let mut reader = Reader::new(b"ab");
        assert_eq!(reader.take(2).expect("in range"), b"ab");
        assert!(matches!(reader.take(1), Err(ProtocolError::Truncated)));
    }

    /// The overflow guard: a length near `usize::MAX` must not wrap into a small
    /// end that slices successfully.
    #[test]
    fn a_reader_does_not_wrap_on_a_hostile_length() {
        let mut reader = Reader::new(b"ab");
        assert!(matches!(
            reader.take(usize::MAX),
            Err(ProtocolError::Truncated)
        ));
    }
}
