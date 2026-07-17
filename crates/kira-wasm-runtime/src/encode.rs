//! Byte-level encoding primitives for the WebAssembly binary format.
//!
//! Everything the module writer emits bottoms out here: LEB128 integers, the
//! length-prefixed vectors and names the format is built from, and the section
//! framing. Kept deliberately small and total — no encoder here can fail, so
//! the fallible parts of a build stay in lowering where the reasons live.

/// A growable byte buffer with the format's encodings on it.
#[derive(Debug, Default, Clone)]
pub struct Bytes {
    inner: Vec<u8>,
}

impl Bytes {
    /// Creates an empty buffer.
    pub fn new() -> Self {
        Self::default()
    }

    /// The bytes written so far.
    pub fn as_slice(&self) -> &[u8] {
        &self.inner
    }

    /// Consumes the buffer, yielding its bytes.
    pub fn into_vec(self) -> Vec<u8> {
        self.inner
    }

    /// How many bytes have been written.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Appends one raw byte.
    pub fn byte(&mut self, value: u8) {
        self.inner.push(value);
    }

    /// Appends raw bytes.
    pub fn raw(&mut self, values: &[u8]) {
        self.inner.extend_from_slice(values);
    }

    /// Appends an unsigned LEB128 integer.
    pub fn u32(&mut self, mut value: u32) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.inner.push(byte);
            if value == 0 {
                return;
            }
        }
    }

    /// Appends an unsigned LEB128 64-bit integer.
    ///
    /// Memory64 widens the operands the format spells as `u32` under Memory32 —
    /// memory limits and load/store offsets — to 64 bits, so both widths are
    /// encoded here rather than at the call sites that must pick one.
    pub fn u64(&mut self, mut value: u64) {
        loop {
            let mut byte = (value & 0x7f) as u8;
            value >>= 7;
            if value != 0 {
                byte |= 0x80;
            }
            self.inner.push(byte);
            if value == 0 {
                return;
            }
        }
    }

    /// Appends a signed LEB128 32-bit integer.
    pub fn i32(&mut self, value: i32) {
        self.signed(i64::from(value));
    }

    /// Appends a signed LEB128 64-bit integer.
    pub fn i64(&mut self, value: i64) {
        self.signed(value);
    }

    /// Appends an IEEE-754 double in the format's little-endian byte order.
    pub fn f64(&mut self, value: f64) {
        self.inner.extend_from_slice(&value.to_le_bytes());
    }

    /// Appends a name: its UTF-8 byte length, then its bytes.
    pub fn name(&mut self, value: &str) {
        self.u32(value.len() as u32);
        self.inner.extend_from_slice(value.as_bytes());
    }

    /// Appends `payload` prefixed by its byte length — the format's vector and
    /// section body shape.
    pub fn sized(&mut self, payload: &Bytes) {
        self.u32(payload.len() as u32);
        self.inner.extend_from_slice(payload.as_slice());
    }

    /// Appends a section: its id, then its length-prefixed body.
    ///
    /// An empty body still writes the section; callers skip sections they do
    /// not want rather than relying on emptiness meaning absence.
    pub fn section(&mut self, id: u8, body: &Bytes) {
        self.inner.push(id);
        self.sized(body);
    }

    /// The signed LEB128 encoding shared by the signed integer widths.
    fn signed(&mut self, mut value: i64) {
        loop {
            let byte = (value & 0x7f) as u8;
            // An arithmetic shift keeps the sign bits coming, so the loop ends
            // on all-ones for a negative value and all-zeros for a positive.
            value >>= 7;
            let sign_bit_set = byte & 0x40 != 0;
            let done = (value == 0 && !sign_bit_set) || (value == -1 && sign_bit_set);
            self.inner.push(if done { byte } else { byte | 0x80 });
            if done {
                return;
            }
        }
    }
}

/// The section ids of the binary format, in their canonical order.
pub mod section {
    /// Function signatures.
    pub const TYPE: u8 = 1;
    /// Imported functions, tables, memories, and globals.
    pub const IMPORT: u8 = 2;
    /// The signature of each defined function.
    pub const FUNCTION: u8 = 3;
    /// The module's memories.
    pub const MEMORY: u8 = 5;
    /// The module's globals.
    pub const GLOBAL: u8 = 6;
    /// The module's exports.
    pub const EXPORT: u8 = 7;
    /// The bodies of each defined function.
    pub const CODE: u8 = 10;
    /// Initialized memory ranges.
    pub const DATA: u8 = 11;
}

/// A WebAssembly value type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValType {
    /// A 32-bit integer: Kira's `Bool`, and every pointer into linear memory.
    I32,
    /// A 64-bit integer: Kira's `Int`.
    I64,
    /// A 64-bit float: Kira's `Float`.
    F64,
}

impl ValType {
    /// The type's byte in the binary format.
    pub fn code(self) -> u8 {
        match self {
            Self::I32 => 0x7f,
            Self::I64 => 0x7e,
            Self::F64 => 0x7c,
        }
    }
}

/// The magic and version every module starts with.
pub const HEADER: [u8; 8] = [0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsigned_leb128_matches_the_specs_examples() {
        let cases: [(u32, &[u8]); 5] = [
            (0, &[0x00]),
            (1, &[0x01]),
            (127, &[0x7f]),
            (128, &[0x80, 0x01]),
            (624_485, &[0xe5, 0x8e, 0x26]),
        ];
        for (value, expected) in cases {
            let mut bytes = Bytes::new();
            bytes.u32(value);
            assert_eq!(bytes.as_slice(), expected, "u32 {value}");
        }
    }

    #[test]
    fn signed_leb128_round_trips_the_edges() {
        let cases: [(i64, &[u8]); 6] = [
            (0, &[0x00]),
            (1, &[0x01]),
            (-1, &[0x7f]),
            (63, &[0x3f]),
            (64, &[0xc0, 0x00]),
            (-64, &[0x40]),
        ];
        for (value, expected) in cases {
            let mut bytes = Bytes::new();
            bytes.i64(value);
            assert_eq!(bytes.as_slice(), expected, "i64 {value}");
        }
    }

    #[test]
    fn signed_leb128_encodes_the_64_bit_extremes() {
        // The extremes are where an arithmetic-shift bug shows up: i64::MIN
        // must not terminate early, and i64::MAX must not sign-extend.
        for value in [i64::MIN, i64::MAX] {
            let mut bytes = Bytes::new();
            bytes.i64(value);
            assert_eq!(decode_signed(bytes.as_slice()), value);
        }
    }

    /// Decodes a signed LEB128 value, mirroring the encoder for its tests.
    fn decode_signed(bytes: &[u8]) -> i64 {
        let mut result: i64 = 0;
        let mut shift = 0;
        for (index, byte) in bytes.iter().enumerate() {
            result |= i64::from(byte & 0x7f) << shift;
            shift += 7;
            if byte & 0x80 == 0 {
                assert_eq!(index, bytes.len() - 1, "trailing bytes after the end");
                if shift < 64 && byte & 0x40 != 0 {
                    result |= -1i64 << shift;
                }
                break;
            }
        }
        result
    }

    #[test]
    fn unsigned_leb128_widens_to_64_bits() {
        let mut bytes = Bytes::new();
        bytes.u64(u64::from(u32::MAX) + 1);
        assert_eq!(bytes.as_slice(), &[0x80, 0x80, 0x80, 0x80, 0x10]);
    }

    #[test]
    fn a_name_is_length_prefixed_utf8() {
        let mut bytes = Bytes::new();
        bytes.name("print");
        assert_eq!(bytes.as_slice(), b"\x05print");
    }

    #[test]
    fn a_section_frames_its_body_with_a_length() {
        let mut body = Bytes::new();
        body.byte(0xaa);
        body.byte(0xbb);
        let mut module = Bytes::new();
        module.section(section::TYPE, &body);
        assert_eq!(module.as_slice(), &[section::TYPE, 0x02, 0xaa, 0xbb]);
    }
}
