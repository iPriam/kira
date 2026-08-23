//! Bounds-checked hybrid-manifest cursor.

use super::ManifestDecodeError;

/// A bounds-checked cursor over a serialized manifest.
pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    /// Creates a cursor at the start of `bytes`.
    pub(super) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, pos: 0 }
    }

    pub(super) fn take(&mut self, count: usize) -> Result<&'a [u8], ManifestDecodeError> {
        let end = self
            .pos
            .checked_add(count)
            .ok_or(ManifestDecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.pos..end)
            .ok_or(ManifestDecodeError::Truncated)?;
        self.pos = end;
        Ok(slice)
    }

    pub(super) fn byte(&mut self) -> Result<u8, ManifestDecodeError> {
        Ok(self.take(1)?[0])
    }

    pub(super) fn is_at_end(&self) -> bool {
        self.pos >= self.bytes.len()
    }

    pub(super) fn u32(&mut self) -> Result<u32, ManifestDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Reads a count that is about to size an allocation, and rejects one the
    /// input could not possibly satisfy.
    ///
    /// Every element of every counted run in this format costs at least one
    /// byte, so a count larger than the bytes remaining is malformed however
    /// the rest of the stream reads. Checking it here is what keeps a
    /// `Vec::with_capacity` off a number the artifact chose: one corrupted byte
    /// in the high end of a count is two billion elements, and reserving for
    /// them aborts the process on a host that will not overcommit — a decoder
    /// killing its caller instead of returning the typed error every other
    /// malformed byte gets.
    pub(super) fn count(&mut self) -> Result<usize, ManifestDecodeError> {
        let count = self.u32()? as usize;
        let remaining = self.bytes.len().saturating_sub(self.pos);
        if count > remaining {
            return Err(ManifestDecodeError::CountExceedsInput { count, remaining });
        }
        Ok(count)
    }

    pub(super) fn string(&mut self) -> Result<String, ManifestDecodeError> {
        let length = self.u32()? as usize;
        let bytes = self.take(length)?;
        String::from_utf8(bytes.to_vec()).map_err(|_| ManifestDecodeError::InvalidString)
    }

    /// Bytes not consumed yet.
    pub(super) fn remaining(&self) -> usize {
        self.bytes.len().saturating_sub(self.pos)
    }
}
