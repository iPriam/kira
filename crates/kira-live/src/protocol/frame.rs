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
