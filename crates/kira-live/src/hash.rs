//! Content hashing for bundle payloads: SHA-256 and the [`ContentHash`] newtype.
//!
//! A bundle names every payload by its content hash, and a reload decision later
//! asks whether a rebuilt payload is byte-identical to the loaded one. That makes
//! the hash a correctness boundary, not a cache key: a collision would let a
//! changed native library pass as unchanged and be hot-patched across an ABI
//! change, which corrupts memory silently. So this is a real collision-resistant
//! hash rather than a short checksum.
//!
//! It is implemented here rather than pulled from a crate because the workspace
//! treats dependencies as frozen, and SHA-256 is a fixed, testable spec: the
//! tests below are the FIPS 180-4 vectors.

use core::fmt;

/// The SHA-256 round constants (FIPS 180-4 §4.2.2).
const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// The SHA-256 initial hash value (FIPS 180-4 §5.3.3).
const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

/// The number of bytes in a [`ContentHash`].
pub const HASH_LEN: usize = 32;

/// The SHA-256 digest of a payload's bytes.
///
/// Two payloads are byte-identical if and only if their hashes match, up to
/// SHA-256's collision resistance — which is what a reload decision relies on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ContentHash([u8; HASH_LEN]);

impl ContentHash {
    /// Hashes `bytes`.
    pub fn of(bytes: &[u8]) -> ContentHash {
        ContentHash(sha256(bytes))
    }

    /// The digest's raw bytes.
    pub fn as_bytes(&self) -> &[u8; HASH_LEN] {
        &self.0
    }

    /// Rebuilds a hash from raw digest bytes read back off the wire.
    pub fn from_bytes(bytes: [u8; HASH_LEN]) -> ContentHash {
        ContentHash(bytes)
    }
}

/// Renders the digest as lowercase hex, which is how it appears in events.
impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// Computes the SHA-256 digest of `input` (FIPS 180-4).
fn sha256(input: &[u8]) -> [u8; HASH_LEN] {
    let mut state = H0;

    // Every whole 64-byte block of the input compresses directly.
    let (blocks, tail) = input.as_chunks::<64>();
    for block in blocks {
        compress(&mut state, block);
    }

    // The tail plus padding: 0x80, then zeroes, then the bit length as a big-endian
    // u64. That needs one final block, or two when the tail leaves no room for the
    // length.
    let mut block = [0u8; 64];
    block[..tail.len()].copy_from_slice(tail);
    block[tail.len()] = 0x80;
    let bit_len = (input.len() as u64).wrapping_mul(8);
    if tail.len() + 1 + 8 > 64 {
        compress(&mut state, &block);
        block = [0u8; 64];
    }
    block[56..].copy_from_slice(&bit_len.to_be_bytes());
    compress(&mut state, &block);

    let mut out = [0u8; HASH_LEN];
    for (slot, word) in out.as_chunks_mut::<4>().0.iter_mut().zip(state) {
        *slot = word.to_be_bytes();
    }
    out
}

/// Compresses one 64-byte block into `state` (FIPS 180-4 §6.2.2).
fn compress(state: &mut [u32; 8], block: &[u8; 64]) {
    let mut w = [0u32; 64];
    for (slot, bytes) in w[..16].iter_mut().zip(block.as_chunks::<4>().0) {
        *slot = u32::from_be_bytes(*bytes);
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }

    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }

    for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *slot = slot.wrapping_add(value);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The FIPS 180-4 one-block vector.
    #[test]
    fn hashes_the_abc_vector() {
        assert_eq!(
            ContentHash::of(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    /// The FIPS 180-4 empty-input vector: padding alone, no message bytes.
    #[test]
    fn hashes_the_empty_vector() {
        assert_eq!(
            ContentHash::of(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    /// The FIPS 180-4 two-block vector: 56 bytes, so the length spills into a
    /// second padding block. This is the case the `tail + 1 + 8 > 64` branch
    /// exists for, and the one a naive implementation gets wrong.
    #[test]
    fn hashes_the_two_block_vector() {
        assert_eq!(
            ContentHash::of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq")
                .to_string(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// A 64-byte input is exactly one block, so the padding block is entirely
    /// padding — the other fencepost around `chunks_exact`.
    #[test]
    fn hashes_exactly_one_block() {
        assert_eq!(
            ContentHash::of(&[b'a'; 64]).to_string(),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    /// The million-`a` vector, which exercises many blocks and a large bit length.
    #[test]
    fn hashes_the_long_vector() {
        assert_eq!(
            ContentHash::of(&[b'a'; 1_000_000]).to_string(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(ContentHash::of(b"payload-a"), ContentHash::of(b"payload-b"));
    }

    #[test]
    fn hash_round_trips_through_raw_bytes() {
        let hash = ContentHash::of(b"bundle");
        assert_eq!(ContentHash::from_bytes(*hash.as_bytes()), hash);
    }
}
