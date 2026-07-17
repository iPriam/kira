//! The `.klbundle` artifact boundary: the `KLB1` manifest and its payloads.
//!
//! A `.klbundle` is the only thing a runner ever consumes. It carries the
//! payloads a runner loads (bytecode, a native library, a hybrid manifest,
//! assets), each named by its [`ContentHash`], plus the platform metadata that
//! says which runner and profile the bundle was built for. A runner never
//! reaches into compiler internals — it reads this and nothing else, which is
//! what lets the compiler's insides change without breaking every runner.
//!
//! The manifest has a self-describing byte format ([`BundleManifest::to_bytes`] /
//! [`BundleManifest::from_bytes`]) behind the `KLB1` magic. Like every wire
//! format here it is append-only, and its decoder validates rather than trusts:
//! a bundle arrives over a socket, so every truncation, every unknown tag, and
//! every out-of-range index is a typed error and none of them panic.

pub mod names;

use std::collections::HashSet;

use kira_manifest::{BuildProfile, RunnerId};

use crate::hash::{ContentHash, HASH_LEN};

pub(crate) use names::is_plain_file_name;

/// The magic bytes that open a serialized bundle manifest: "KLB1".
pub const MAGIC: [u8; 4] = *b"KLB1";

/// The file a `.klbundle` directory's manifest is written to.
pub const MANIFEST_FILE: &str = "manifest.klb";

/// The subdirectory a `.klbundle` directory's payloads are written under.
pub const PAYLOAD_DIR: &str = "payloads";

/// What a payload is, and therefore what a runner does with it.
///
/// A runner that meets a kind it does not handle reports that precisely rather
/// than guessing: an unknown payload is never silently skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayloadKind {
    /// A `KBC1` bytecode module, run by the VM.
    VmBytecode,
    /// A native dynamic library, loaded and linked by the runner.
    NativeLibrary,
    /// A `KHM1` hybrid manifest, pairing a bytecode module with a native half.
    HybridManifest,
    /// An opaque resource the app reads at runtime.
    Asset,
}

impl PayloadKind {
    /// This kind's wire byte.
    ///
    /// Append-only: a new kind takes the next free byte and never renumbers an
    /// existing one, because bundles already on disk are decoded by this.
    pub fn as_byte(self) -> u8 {
        match self {
            Self::VmBytecode => 0,
            Self::NativeLibrary => 1,
            Self::HybridManifest => 2,
            Self::Asset => 3,
        }
    }

    /// The kind a wire byte names, or `None` if this build knows no such kind.
    pub fn from_byte(byte: u8) -> Option<PayloadKind> {
        match byte {
            0 => Some(Self::VmBytecode),
            1 => Some(Self::NativeLibrary),
            2 => Some(Self::HybridManifest),
            3 => Some(Self::Asset),
            _ => None,
        }
    }

    /// A short label for diagnostics and events.
    pub fn label(self) -> &'static str {
        match self {
            Self::VmBytecode => "vm-bytecode",
            Self::NativeLibrary => "native-library",
            Self::HybridManifest => "hybrid-manifest",
            Self::Asset => "asset",
        }
    }

    /// Whether a change to this payload can be applied to a running process
    /// without relaunching it.
    ///
    /// Only the VM's bytecode can: the VM swaps a module between frames while
    /// its heap stays put. Native code cannot be swapped in place — the loaded
    /// library's code and layouts are already baked into the running process —
    /// so a change to one forces a relaunch. This is the fact the reload tier
    /// decision is built on, and it belongs to the payload kind rather than to
    /// the supervisor that asks.
    pub fn is_hot_swappable(self) -> bool {
        match self {
            Self::VmBytecode => true,
            Self::NativeLibrary | Self::HybridManifest | Self::Asset => false,
        }
    }
}

/// One payload inside a bundle: what it is, what it is called, and what it hashes to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PayloadEntry {
    /// The payload's name, unique within the bundle. Also its file name under
    /// [`PAYLOAD_DIR`], so it is validated on decode to stay a plain file name.
    pub name: String,
    /// What the payload is.
    pub kind: PayloadKind,
    /// The hash of the payload's bytes.
    pub hash: ContentHash,
    /// The payload's length in bytes.
    pub size: u64,
}

/// A bundle's manifest: its platform metadata, its payloads, and its entrypoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleManifest {
    /// The runner this bundle was built for.
    pub runner: RunnerId,
    /// The profile it was built at.
    pub profile: BuildProfile,
    /// The payloads, in the order a runner should load them.
    pub payloads: Vec<PayloadEntry>,
    /// Index into [`BundleManifest::payloads`] of the entrypoint payload — the
    /// one whose start is the app starting.
    pub entry: u32,
}

/// An error decoding a serialized bundle manifest.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BundleDecodeError {
    /// The stream did not begin with the `KLB1` magic.
    #[error("not a Kira live bundle (bad magic)")]
    BadMagic,
    /// The stream ended before a full manifest was read.
    #[error("truncated bundle manifest")]
    Truncated,
    /// A string in the manifest was not valid UTF-8.
    #[error("invalid UTF-8 in bundle manifest")]
    InvalidString,
    /// A runner byte named no runner this build knows.
    #[error("unknown runner `{0}` in bundle manifest")]
    UnknownRunner(u8),
    /// A profile byte named no profile this build knows.
    #[error("unknown build profile `{0}` in bundle manifest")]
    UnknownProfile(u8),
    /// A payload kind byte named no kind this build knows.
    #[error("unknown payload kind `{0}` in bundle manifest")]
    UnknownPayloadKind(u8),
    /// The entry index did not name a payload in the bundle.
    #[error("bundle entry index {entry} names no payload (bundle has {count})")]
    EntryOutOfRange {
        /// The out-of-range index the manifest carried.
        entry: u32,
        /// How many payloads the bundle actually has.
        count: usize,
    },
    /// A bundle with no payloads has nothing to start.
    #[error("bundle has no payloads")]
    NoPayloads,
    /// Two payloads shared a name, so a lookup by name would be ambiguous.
    #[error("duplicate payload name `{0}` in bundle manifest")]
    DuplicatePayloadName(String),
    /// A payload name was not a plain file name.
    ///
    /// A bundle arrives over a socket and its payload names become paths on
    /// disk, so a name carrying a separator or `..` would let a hostile bundle
    /// write outside the bundle directory. Rejected at the decoder, once, rather
    /// than at each of the places that later joins a path.
    #[error("payload name `{0}` is not a plain file name")]
    UnsafePayloadName(String),
}

impl BundleManifest {
    /// Serializes the manifest to its byte format.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.push(runner_byte(self.runner));
        out.push(profile_byte(self.profile));
        out.extend_from_slice(&self.entry.to_le_bytes());
        write_u32(&mut out, self.payloads.len() as u32);
        for payload in &self.payloads {
            write_bytes(&mut out, payload.name.as_bytes());
            out.push(payload.kind.as_byte());
            out.extend_from_slice(payload.hash.as_bytes());
            out.extend_from_slice(&payload.size.to_le_bytes());
        }
        out
    }

    /// Deserializes a manifest from its byte format, validating every field.
    pub fn from_bytes(bytes: &[u8]) -> Result<BundleManifest, BundleDecodeError> {
        let mut reader = Reader { bytes, offset: 0 };
        if reader.take(4)? != MAGIC {
            return Err(BundleDecodeError::BadMagic);
        }
        let runner_raw = reader.take(1)?[0];
        let runner =
            runner_from_byte(runner_raw).ok_or(BundleDecodeError::UnknownRunner(runner_raw))?;
        let profile_raw = reader.take(1)?[0];
        let profile =
            profile_from_byte(profile_raw).ok_or(BundleDecodeError::UnknownProfile(profile_raw))?;
        let entry = reader.read_u32()?;

        let payload_count = reader.read_u32()?;
        let mut payloads: Vec<PayloadEntry> = Vec::new();
        // Names seen so far, for the duplicate check. A linear scan per payload
        // would make decoding quadratic in a count the peer chooses: the frame
        // limit bounds a manifest's *bytes*, but a few megabytes of one-character
        // names is millions of entries, and n² of those is hours of CPU inside a
        // decoder that is supposed to reject hostile input, not chew on it.
        let mut seen: HashSet<String> = HashSet::new();
        for _ in 0..payload_count {
            let name = reader.read_string()?;
            if !is_plain_file_name(&name) {
                return Err(BundleDecodeError::UnsafePayloadName(name));
            }
            if !seen.insert(name.clone()) {
                return Err(BundleDecodeError::DuplicatePayloadName(name));
            }
            let kind_raw = reader.take(1)?[0];
            let kind = PayloadKind::from_byte(kind_raw)
                .ok_or(BundleDecodeError::UnknownPayloadKind(kind_raw))?;
            let mut digest = [0u8; HASH_LEN];
            digest.copy_from_slice(reader.take(HASH_LEN)?);
            let size = reader.read_u64()?;
            payloads.push(PayloadEntry {
                name,
                kind,
                hash: ContentHash::from_bytes(digest),
                size,
            });
        }

        if payloads.is_empty() {
            return Err(BundleDecodeError::NoPayloads);
        }
        if entry as usize >= payloads.len() {
            return Err(BundleDecodeError::EntryOutOfRange {
                entry,
                count: payloads.len(),
            });
        }

        Ok(BundleManifest {
            runner,
            profile,
            payloads,
            entry,
        })
    }

    /// The entrypoint payload, or `None` if `entry` names no payload.
    ///
    /// This returns an `Option` rather than indexing, because a `BundleManifest`
    /// is a plain struct with public fields: anyone can build one with an entry
    /// that names nothing, and a library does not get to end its caller's process
    /// over it. [`Bundle`](crate::Bundle) is the type that guarantees the entry
    /// is in range — both its constructors check — which is why
    /// [`Bundle::entry_bytes`](crate::Bundle::entry_bytes) can be total and this
    /// cannot.
    pub fn entry_payload(&self) -> Option<&PayloadEntry> {
        self.payloads.get(self.entry as usize)
    }

    /// The payload called `name`, if the bundle has one.
    pub fn payload(&self, name: &str) -> Option<&PayloadEntry> {
        self.payloads.iter().find(|payload| payload.name == name)
    }
}

/// The wire byte for a runner.
///
/// Delegates to [`RunnerId::index`] so the wire ordering and the resolved-matrix
/// ordering are one mapping rather than two that must be kept agreeing. The test
/// below pins the actual byte values, so a reorder fails loudly here instead of
/// silently redirecting bundles to the wrong runner.
fn runner_byte(runner: RunnerId) -> u8 {
    runner.index() as u8
}

/// The runner a wire byte names, or `None` if this build knows no such runner.
fn runner_from_byte(byte: u8) -> Option<RunnerId> {
    RunnerId::all().get(byte as usize).copied()
}

/// The wire byte for a build profile; see [`runner_byte`] for why this indexes.
fn profile_byte(profile: BuildProfile) -> u8 {
    profile.index() as u8
}

/// The profile a wire byte names, or `None` if this build knows no such profile.
fn profile_from_byte(byte: u8) -> Option<BuildProfile> {
    BuildProfile::all().get(byte as usize).copied()
}

fn write_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn write_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    write_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

/// A bounds-checked cursor over a manifest's bytes.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], BundleDecodeError> {
        // Checked rather than `self.offset + n`: a hostile length would overflow
        // and wrap to a small end that slices successfully.
        let end = self
            .offset
            .checked_add(n)
            .ok_or(BundleDecodeError::Truncated)?;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(BundleDecodeError::Truncated)?;
        self.offset = end;
        Ok(slice)
    }

    fn read_u32(&mut self) -> Result<u32, BundleDecodeError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_u64(&mut self) -> Result<u64, BundleDecodeError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    fn read_len_prefixed(&mut self) -> Result<&'a [u8], BundleDecodeError> {
        let len = self.read_u32()? as usize;
        self.take(len)
    }

    fn read_string(&mut self) -> Result<String, BundleDecodeError> {
        let bytes = self.read_len_prefixed()?;
        core::str::from_utf8(bytes)
            .map(str::to_owned)
            .map_err(|_| BundleDecodeError::InvalidString)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(name: &str, kind: PayloadKind, bytes: &[u8]) -> PayloadEntry {
        PayloadEntry {
            name: name.to_owned(),
            kind,
            hash: ContentHash::of(bytes),
            size: bytes.len() as u64,
        }
    }

    fn manifest() -> BundleManifest {
        BundleManifest {
            runner: RunnerId::Desktop,
            profile: BuildProfile::Debug,
            payloads: vec![
                payload("app.kbc", PayloadKind::VmBytecode, b"KBC1...."),
                payload("libapp.dylib", PayloadKind::NativeLibrary, b"\x7fELF"),
            ],
            entry: 0,
        }
    }

    #[test]
    fn manifest_round_trips_through_bytes() {
        let manifest = manifest();
        let bytes = manifest.to_bytes();
        assert_eq!(BundleManifest::from_bytes(&bytes).unwrap(), manifest);
    }

    #[test]
    fn every_runner_and_profile_round_trips() {
        for runner in RunnerId::all() {
            for profile in BuildProfile::all() {
                let manifest = BundleManifest {
                    runner,
                    profile,
                    ..manifest()
                };
                let decoded = BundleManifest::from_bytes(&manifest.to_bytes()).unwrap();
                assert_eq!(decoded.runner, runner);
                assert_eq!(decoded.profile, profile);
            }
        }
    }

    #[test]
    fn every_payload_kind_round_trips() {
        for kind in [
            PayloadKind::VmBytecode,
            PayloadKind::NativeLibrary,
            PayloadKind::HybridManifest,
            PayloadKind::Asset,
        ] {
            assert_eq!(PayloadKind::from_byte(kind.as_byte()), Some(kind));
        }
    }

    /// The wire bytes are pinned literally. `runner_byte` delegates to
    /// `RunnerId::index`, so without this a reorder of that matrix would silently
    /// change what every existing bundle on disk decodes to.
    #[test]
    fn runner_wire_bytes_are_pinned() {
        let expected = [
            (RunnerId::Desktop, 0u8),
            (RunnerId::Macos, 1),
            (RunnerId::Ios, 2),
            (RunnerId::Tvos, 3),
            (RunnerId::Visionos, 4),
            (RunnerId::Windows, 5),
            (RunnerId::Android, 6),
            (RunnerId::Web, 7),
            (RunnerId::Linux, 8),
        ];
        for (runner, byte) in expected {
            assert_eq!(runner_byte(runner), byte, "wire byte for {runner:?}");
            assert_eq!(runner_from_byte(byte), Some(runner));
        }
    }

    /// The same pinning for profiles, for the same reason.
    #[test]
    fn profile_wire_bytes_are_pinned() {
        let expected = [
            (BuildProfile::Debug, 0u8),
            (BuildProfile::Profiler, 1),
            (BuildProfile::Release, 2),
        ];
        for (profile, byte) in expected {
            assert_eq!(profile_byte(profile), byte, "wire byte for {profile:?}");
            assert_eq!(profile_from_byte(byte), Some(profile));
        }
    }

    /// Only bytecode may be hot-patched into a running process. If this ever
    /// reads otherwise the reload tier decision starts swapping native code
    /// under a live process, so it is pinned rather than left to inspection.
    #[test]
    fn only_bytecode_is_hot_swappable() {
        assert!(PayloadKind::VmBytecode.is_hot_swappable());
        assert!(!PayloadKind::NativeLibrary.is_hot_swappable());
        assert!(!PayloadKind::HybridManifest.is_hot_swappable());
        assert!(!PayloadKind::Asset.is_hot_swappable());
    }

    #[test]
    fn bad_magic_is_rejected() {
        assert_eq!(
            BundleManifest::from_bytes(b"XXXX").unwrap_err(),
            BundleDecodeError::BadMagic
        );
    }

    /// Every prefix of a valid manifest is truncated, and none of them panic.
    #[test]
    fn every_truncation_is_rejected() {
        let bytes = manifest().to_bytes();
        for len in 0..bytes.len() {
            let error = BundleManifest::from_bytes(&bytes[..len])
                .expect_err("a truncated manifest must not decode");
            assert!(
                matches!(
                    error,
                    BundleDecodeError::Truncated | BundleDecodeError::BadMagic
                ),
                "prefix of {len} bytes gave {error:?}"
            );
        }
    }

    #[test]
    fn unknown_runner_is_rejected() {
        let mut bytes = manifest().to_bytes();
        bytes[4] = 200;
        assert_eq!(
            BundleManifest::from_bytes(&bytes).unwrap_err(),
            BundleDecodeError::UnknownRunner(200)
        );
    }

    #[test]
    fn unknown_profile_is_rejected() {
        let mut bytes = manifest().to_bytes();
        bytes[5] = 9;
        assert_eq!(
            BundleManifest::from_bytes(&bytes).unwrap_err(),
            BundleDecodeError::UnknownProfile(9)
        );
    }

    #[test]
    fn unknown_payload_kind_is_rejected() {
        let manifest = BundleManifest {
            payloads: vec![payload("p", PayloadKind::VmBytecode, b"x")],
            entry: 0,
            ..manifest()
        };
        let mut bytes = manifest.to_bytes();
        // magic(4) runner(1) profile(1) entry(4) count(4) name_len(4) name(1) -> kind
        let kind_at = 4 + 1 + 1 + 4 + 4 + 4 + 1;
        bytes[kind_at] = 250;
        assert_eq!(
            BundleManifest::from_bytes(&bytes).unwrap_err(),
            BundleDecodeError::UnknownPayloadKind(250)
        );
    }

    #[test]
    fn an_out_of_range_entry_is_rejected() {
        let manifest = BundleManifest {
            entry: 7,
            ..manifest()
        };
        assert_eq!(
            BundleManifest::from_bytes(&manifest.to_bytes()).unwrap_err(),
            BundleDecodeError::EntryOutOfRange { entry: 7, count: 2 }
        );
    }

    #[test]
    fn a_bundle_with_no_payloads_is_rejected() {
        let manifest = BundleManifest {
            payloads: Vec::new(),
            entry: 0,
            ..manifest()
        };
        assert_eq!(
            BundleManifest::from_bytes(&manifest.to_bytes()).unwrap_err(),
            BundleDecodeError::NoPayloads
        );
    }

    #[test]
    fn duplicate_payload_names_are_rejected() {
        let manifest = BundleManifest {
            payloads: vec![
                payload("same", PayloadKind::VmBytecode, b"a"),
                payload("same", PayloadKind::Asset, b"b"),
            ],
            entry: 0,
            ..manifest()
        };
        assert_eq!(
            BundleManifest::from_bytes(&manifest.to_bytes()).unwrap_err(),
            BundleDecodeError::DuplicatePayloadName("same".to_owned())
        );
    }

    /// A payload name becomes a path under the bundle directory, so a name that
    /// escapes it is rejected at the decoder. A bundle is attacker-reachable: it
    /// arrives over a socket.
    #[test]
    fn traversing_payload_names_are_rejected() {
        // Which names are unsafe is `names`' business and is tested there. This
        // is the decoder's half of the contract: that it actually applies the
        // rule, and reports it as an unsafe name rather than some other error.
        for name in ["../escape", "sub/dir", "C:evil.dll", "CON", ""] {
            let manifest = BundleManifest {
                payloads: vec![payload(name, PayloadKind::Asset, b"x")],
                entry: 0,
                ..manifest()
            };
            assert_eq!(
                BundleManifest::from_bytes(&manifest.to_bytes()).unwrap_err(),
                BundleDecodeError::UnsafePayloadName(name.to_owned()),
                "name `{name}` must be rejected"
            );
        }
    }

    /// A hostile payload count must not make the decoder try to reserve for it.
    #[test]
    fn a_huge_payload_count_does_not_allocate() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&MAGIC);
        bytes.push(0);
        bytes.push(0);
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(
            BundleManifest::from_bytes(&bytes).unwrap_err(),
            BundleDecodeError::Truncated
        );
    }

    #[test]
    fn entry_payload_is_the_indexed_one() {
        let manifest = BundleManifest {
            entry: 1,
            ..manifest()
        };
        assert_eq!(
            manifest.entry_payload().expect("entry is in range").name,
            "libapp.dylib"
        );
    }

    /// A hand-built manifest can name an entry that is not there, and asking for
    /// it says so rather than panicking. `Bundle` is what makes the entry total;
    /// a bare manifest does not.
    #[test]
    fn an_entry_naming_nothing_is_none_not_a_panic() {
        let manifest = BundleManifest {
            entry: 99,
            ..manifest()
        };
        assert!(manifest.entry_payload().is_none());
    }

    #[test]
    fn payload_lookup_finds_by_name() {
        let manifest = manifest();
        assert_eq!(
            manifest.payload("app.kbc").unwrap().kind,
            PayloadKind::VmBytecode
        );
        assert!(manifest.payload("absent").is_none());
    }
}
