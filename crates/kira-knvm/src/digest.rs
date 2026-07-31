//! SHA-256, and the sidecar file a published artifact is verified against.
//!
//! # Why the hash is implemented here
//!
//! For the same reason the transport is a `curl` subprocess: the workspace's
//! external dependency set is deliberately frozen, and pulling a digest crate
//! in for one function is the larger commitment. SHA-256 is a fixed algorithm
//! with published test vectors, so the implementation below is pinned by those
//! vectors rather than trusted — [`tests`] checks the three NIST cases and the
//! block boundaries a streaming hasher gets wrong.
//!
//! # What a checksum is worth here
//!
//! The transport is HTTPS, so this is not what stands between a user and a
//! hostile network — TLS is. What it catches is a truncated or corrupted
//! transfer, a mirror serving stale bytes, and an artifact repackaged after
//! publication. That is worth a hard refusal, and it is why a *mismatch* fails
//! the install while an *absent* sidecar only downgrades it to unverified: a
//! release published before the sidecar existed must stay installable, and an
//! attacker able to delete the sidecar from a TLS-served release already owns
//! the archive beside it.

use std::io::Read;
use std::path::Path;

/// The bytes read per `read` call while hashing a file.
///
/// A release archive is tens of megabytes; hashing it must not read it into
/// memory whole, and must not pay a syscall per kilobyte either.
const READ_CHUNK: usize = 64 * 1024;

/// The round constants: the first 32 bits of the fractional parts of the cube
/// roots of the first 64 primes (FIPS 180-4, §4.2.2).
const ROUND_CONSTANTS: [u32; 64] = [
    0x428a_2f98,
    0x7137_4491,
    0xb5c0_fbcf,
    0xe9b5_dba5,
    0x3956_c25b,
    0x59f1_11f1,
    0x923f_82a4,
    0xab1c_5ed5,
    0xd807_aa98,
    0x1283_5b01,
    0x2431_85be,
    0x550c_7dc3,
    0x72be_5d74,
    0x80de_b1fe,
    0x9bdc_06a7,
    0xc19b_f174,
    0xe49b_69c1,
    0xefbe_4786,
    0x0fc1_9dc6,
    0x240c_a1cc,
    0x2de9_2c6f,
    0x4a74_84aa,
    0x5cb0_a9dc,
    0x76f9_88da,
    0x983e_5152,
    0xa831_c66d,
    0xb003_27c8,
    0xbf59_7fc7,
    0xc6e0_0bf3,
    0xd5a7_9147,
    0x06ca_6351,
    0x1429_2967,
    0x27b7_0a85,
    0x2e1b_2138,
    0x4d2c_6dfc,
    0x5338_0d13,
    0x650a_7354,
    0x766a_0abb,
    0x81c2_c92e,
    0x9272_2c85,
    0xa2bf_e8a1,
    0xa81a_664b,
    0xc24b_8b70,
    0xc76c_51a3,
    0xd192_e819,
    0xd699_0624,
    0xf40e_3585,
    0x106a_a070,
    0x19a4_c116,
    0x1e37_6c08,
    0x2748_774c,
    0x34b0_bcb5,
    0x391c_0cb3,
    0x4ed8_aa4a,
    0x5b9c_ca4f,
    0x682e_6ff3,
    0x748f_82ee,
    0x78a5_636f,
    0x84c8_7814,
    0x8cc7_0208,
    0x90be_fffa,
    0xa450_6ceb,
    0xbef9_a3f7,
    0xc671_78f2,
];

/// The initial hash value: the first 32 bits of the fractional parts of the
/// square roots of the first 8 primes (FIPS 180-4, §5.3.3).
const INITIAL_STATE: [u32; 8] = [
    0x6a09_e667,
    0xbb67_ae85,
    0x3c6e_f372,
    0xa54f_f53a,
    0x510e_527f,
    0x9b05_688c,
    0x1f83_d9ab,
    0x5be0_cd19,
];

/// A SHA-256 digest.
///
/// Compared by value, so a verification is `computed == published` and never a
/// string comparison whose case or whitespace could decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sha256([u8; 32]);

impl Sha256 {
    /// The digest of a byte slice.
    #[must_use]
    pub fn of(bytes: &[u8]) -> Self {
        let mut hasher = Hasher::new();
        hasher.update(bytes);
        hasher.finish()
    }

    /// The digest of a file, read in chunks rather than into memory whole.
    pub fn of_file(path: &Path) -> Result<Self, std::io::Error> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Hasher::new();
        let mut buffer = vec![0_u8; READ_CHUNK];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                return Ok(hasher.finish());
            }
            hasher.update(&buffer[..read]);
        }
    }

    /// Reads a digest from its lowercase or uppercase hex spelling.
    ///
    /// Returns `None` for anything that is not exactly 64 hex digits, so a
    /// truncated or annotated field is a refusal rather than a partial parse.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0_u8; 32];
        let digits = text.as_bytes();
        for (index, byte) in bytes.iter_mut().enumerate() {
            let high = hex_value(digits[index * 2])?;
            let low = hex_value(digits[index * 2 + 1])?;
            *byte = (high << 4) | low;
        }
        Some(Self(bytes))
    }

    /// The digest's bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl std::fmt::Display for Sha256 {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

/// The value of one hex digit, or `None` when it is not one.
fn hex_value(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        b'A'..=b'F' => Some(digit - b'A' + 10),
        _ => None,
    }
}

/// The name of the sidecar file carrying an artifact's published digest.
///
/// `<artifact>.sha256`, holding what `shasum -a 256` writes: the hex digest,
/// optionally followed by the file name it was computed over.
#[must_use]
pub fn checksum_file_name(artifact_name: &str) -> String {
    format!("{artifact_name}.sha256")
}

/// Reads the digest out of a sidecar file's contents.
///
/// Accepts both a bare digest and the `<digest>  <name>` line `shasum` and
/// `sha256sum` write, so the sidecar can be produced by either. The file name
/// half is ignored on purpose: the caller already knows which artifact it
/// fetched, and a sidecar naming a different one is a publishing error that
/// comparing names would report as a mismatch instead of as what it is.
#[must_use]
pub fn parse_checksum_file(contents: &str) -> Option<Sha256> {
    contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| {
            let field = line.split_whitespace().next()?;
            // GNU `sha256sum` marks a binary-mode digest with a `*` before the
            // file name, never before the digest; a leading `\` marks an
            // escaped file name. Neither belongs to the digest field itself.
            Sha256::parse_hex(field.trim_start_matches('\\'))
        })
}

/// The streaming SHA-256 state.
///
/// Private: callers hash a slice or a file, and the block bookkeeping is not
/// vocabulary anything outside this module should have to hold.
struct Hasher {
    /// The eight working hash words.
    state: [u32; 8],
    /// Bytes accepted but not yet part of a full 64-byte block.
    buffer: [u8; 64],
    /// How many bytes of `buffer` are live.
    buffered: usize,
    /// The total message length, which the padding encodes in bits.
    length: u64,
}

impl Hasher {
    /// A hasher over the empty message.
    fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0; 64],
            buffered: 0,
            length: 0,
        }
    }

    /// Accepts more of the message.
    fn update(&mut self, mut bytes: &[u8]) {
        self.length = self.length.wrapping_add(bytes.len() as u64);

        if self.buffered > 0 {
            let wanted = (64 - self.buffered).min(bytes.len());
            self.buffer[self.buffered..self.buffered + wanted].copy_from_slice(&bytes[..wanted]);
            self.buffered += wanted;
            bytes = &bytes[wanted..];
            if self.buffered < 64 {
                // Everything given fit inside the partial block, so `bytes` is
                // now empty. Returning here is what keeps the tail below the
                // one place that assigns `buffered`: falling through would
                // reach `self.buffered = remainder.len()` with an empty
                // remainder and silently discard the bytes just buffered.
                return;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }

        let mut chunks = bytes.chunks_exact(64);
        for chunk in &mut chunks {
            let mut block = [0_u8; 64];
            block.copy_from_slice(chunk);
            self.compress(&block);
        }

        let remainder = chunks.remainder();
        self.buffer[..remainder.len()].copy_from_slice(remainder);
        self.buffered = remainder.len();
    }

    /// Pads the message and produces the digest.
    fn finish(mut self) -> Sha256 {
        // FIPS 180-4 §5.1.1: a `0x80` byte, then zeroes up to 56 bytes mod 64,
        // then the message length in bits as a big-endian u64.
        let bit_length = self.length.wrapping_mul(8);
        self.update(&[0x80]);
        while self.buffered != 56 {
            self.update(&[0x00]);
        }
        self.update(&bit_length.to_be_bytes());

        let mut digest = [0_u8; 32];
        for (index, word) in self.state.iter().enumerate() {
            digest[index * 4..index * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        Sha256(digest)
    }

    /// The compression function over one 64-byte block (FIPS 180-4, §6.2.2).
    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0_u32; 64];
        for (index, word) in schedule.iter_mut().take(16).enumerate() {
            let start = index * 4;
            *word = u32::from_be_bytes([
                block[start],
                block[start + 1],
                block[start + 2],
                block[start + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(ROUND_CONSTANTS[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        for (word, addend) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *word = word.wrapping_add(addend);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published FIPS 180-4 vectors. An implementation that passes these
    /// and the boundary cases below is the algorithm, not something like it.
    #[test]
    fn matches_the_published_test_vectors() {
        assert_eq!(
            Sha256::of(b"").to_string(),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            Sha256::of(b"abc").to_string(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            Sha256::of(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq").to_string(),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    /// One million `a` — the vector that catches a length counter that
    /// overflows or is counted in the wrong unit.
    #[test]
    fn matches_the_long_message_vector() {
        let mut hasher = Hasher::new();
        for _ in 0..1000 {
            hasher.update(&[b'a'; 1000]);
        }
        assert_eq!(
            hasher.finish().to_string(),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }

    /// A streaming hasher that is right on whole blocks and wrong across them
    /// is the classic defect, so the same message is fed in every chunking
    /// that straddles the 64-byte boundary.
    #[test]
    fn is_independent_of_how_the_message_is_chunked() {
        let message: Vec<u8> = (0..200_u32).map(|index| (index % 251) as u8).collect();
        let whole = Sha256::of(&message);
        for chunk in [1, 7, 63, 64, 65, 100, 199] {
            let mut hasher = Hasher::new();
            for part in message.chunks(chunk) {
                hasher.update(part);
            }
            assert_eq!(
                hasher.finish(),
                whole,
                "hashing in {chunk}-byte chunks must equal hashing it whole"
            );
        }
    }

    /// The two message lengths whose padding needs an extra block.
    #[test]
    fn pads_the_block_boundary_lengths() {
        assert_eq!(
            Sha256::of(&[b'a'; 55]).to_string(),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            Sha256::of(&[b'a'; 56]).to_string(),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            Sha256::of(&[b'a'; 64]).to_string(),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
    }

    #[test]
    fn round_trips_through_hex() {
        let digest = Sha256::of(b"kira");
        let parsed = Sha256::parse_hex(&digest.to_string()).expect("its own spelling parses");
        assert_eq!(parsed, digest);
        assert_eq!(
            Sha256::parse_hex(&digest.to_string().to_uppercase()),
            Some(digest),
            "a published digest may be uppercase"
        );
    }

    #[test]
    fn refuses_anything_that_is_not_a_whole_digest() {
        assert_eq!(Sha256::parse_hex(""), None);
        assert_eq!(Sha256::parse_hex(&"a".repeat(63)), None);
        assert_eq!(Sha256::parse_hex(&"a".repeat(65)), None);
        assert_eq!(Sha256::parse_hex(&"z".repeat(64)), None);
    }

    #[test]
    fn reads_both_sidecar_spellings() {
        let digest = Sha256::of(b"archive bytes");
        let hex = digest.to_string();
        for contents in [
            format!("{hex}\n"),
            format!("{hex}  kira-1.7.3-aarch64-macos.tar.gz\n"),
            format!("{hex} *kira-1.7.3-aarch64-macos.tar.gz\n"),
            format!("\n\n{hex}  kira.tar.gz\n"),
        ] {
            assert_eq!(
                parse_checksum_file(&contents),
                Some(digest),
                "`{contents:?}` is a sidecar this must read"
            );
        }
    }

    #[test]
    fn refuses_a_sidecar_that_carries_no_digest() {
        assert_eq!(parse_checksum_file(""), None);
        assert_eq!(parse_checksum_file("\n \n"), None);
        assert_eq!(parse_checksum_file("not-a-digest  file.tar.gz"), None);
    }

    #[test]
    fn names_the_sidecar_after_its_artifact() {
        assert_eq!(
            checksum_file_name("kira-1.7.3-aarch64-macos.tar.gz"),
            "kira-1.7.3-aarch64-macos.tar.gz.sha256"
        );
    }

    #[test]
    fn hashes_a_file_exactly_as_it_hashes_its_bytes() {
        let path = std::env::temp_dir().join(format!("knvm_digest_{}", std::process::id()));
        // Larger than one read chunk, so the file path exercises the loop.
        let contents: Vec<u8> = (0..(READ_CHUNK * 2 + 17))
            .map(|index| (index % 253) as u8)
            .collect();
        std::fs::write(&path, &contents).expect("write the fixture file");
        let from_file = Sha256::of_file(&path).expect("hash the fixture file");
        let _ = std::fs::remove_file(&path);
        assert_eq!(from_file, Sha256::of(&contents));
    }
}
