//! Reading and writing a `.klbundle` directory.
//!
//! The layout is the manifest beside a payload directory:
//!
//! ```text
//! app.klbundle/
//!   manifest.klb          the KLB1 manifest
//!   payloads/
//!     app.kbc             one file per payload, named by the manifest
//! ```
//!
//! Writing verifies nothing (the writer just hashed the bytes); reading verifies
//! everything. A bundle read back may have been written by another process,
//! truncated by a crashed build, or handed over by a socket, so
//! [`Bundle::read`] checks each payload against its recorded hash and refuses
//! the bundle rather than handing a runner bytes that are not what the manifest
//! says they are.

use std::fs;
use std::path::{Path, PathBuf};

use crate::bundle::{BundleManifest, MANIFEST_FILE, PAYLOAD_DIR, PayloadEntry};
use crate::hash::ContentHash;

/// A bundle held in memory: its manifest plus the bytes of each payload.
///
/// Payload bytes are stored in the same order as `manifest.payloads`, and the
/// two are only ever constructed together, so a payload's bytes are found by the
/// same index that found its entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bundle {
    manifest: BundleManifest,
    payloads: Vec<Vec<u8>>,
}

/// An error building, writing, or reading a bundle.
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    /// The bundle directory or one of its files could not be read or written.
    #[error("bundle i/o failed at `{path}`: {source}")]
    Io {
        /// The path being read or written.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The manifest did not decode.
    #[error("bundle manifest at `{path}` is invalid: {source}")]
    Manifest {
        /// The manifest's path.
        path: PathBuf,
        /// The decode failure.
        #[source]
        source: crate::bundle::BundleDecodeError,
    },
    /// A payload's bytes did not hash to what the manifest recorded.
    ///
    /// The bundle is refused whole: a runner that loaded the good payloads and
    /// skipped the bad one would be running something no build ever produced.
    #[error(
        "payload `{name}` does not match its manifest hash (expected {expected}, found {found})"
    )]
    HashMismatch {
        /// The payload's name.
        name: String,
        /// The hash the manifest recorded.
        expected: ContentHash,
        /// The hash the bytes on disk actually have.
        found: ContentHash,
    },
    /// A payload's length did not match what the manifest recorded.
    #[error("payload `{name}` is {found} bytes, but its manifest records {expected}")]
    SizeMismatch {
        /// The payload's name.
        name: String,
        /// The size the manifest recorded.
        expected: u64,
        /// The size the bytes on disk actually have.
        found: u64,
    },
    /// A payload named by the manifest was not present in the bundle.
    #[error("payload `{name}` is named by the manifest but missing from the bundle")]
    MissingPayload {
        /// The payload's name.
        name: String,
    },
}

impl Bundle {
    /// Builds a bundle from payloads, hashing each one.
    ///
    /// The manifest is derived from the bytes rather than supplied alongside
    /// them, so a manifest that disagrees with its payloads is unrepresentable
    /// here — the only way to get a mismatch is to corrupt the bytes afterward,
    /// which is exactly what [`Bundle::read`] checks for.
    pub fn build(
        runner: kira_manifest::RunnerId,
        profile: kira_manifest::BuildProfile,
        payloads: Vec<NamedPayload>,
        entry: u32,
    ) -> Bundle {
        let entries = payloads
            .iter()
            .map(|payload| PayloadEntry {
                name: payload.name.clone(),
                kind: payload.kind,
                hash: ContentHash::of(&payload.bytes),
                size: payload.bytes.len() as u64,
            })
            .collect();
        Bundle {
            manifest: BundleManifest {
                runner,
                profile,
                payloads: entries,
                entry,
            },
            payloads: payloads.into_iter().map(|payload| payload.bytes).collect(),
        }
    }

    /// Reassembles a bundle from a manifest and the payload bytes received for
    /// it, verifying each payload against the manifest.
    ///
    /// This is the path a bundle arriving over the wire takes: the manifest came
    /// first, the payloads followed, and nothing has checked that they agree.
    pub fn assemble(
        manifest: BundleManifest,
        mut payloads: Vec<Option<Vec<u8>>>,
    ) -> Result<Bundle, BundleError> {
        payloads.resize_with(manifest.payloads.len(), || None);
        let mut bytes = Vec::with_capacity(manifest.payloads.len());
        for (entry, received) in manifest.payloads.iter().zip(payloads) {
            let received = received.ok_or_else(|| BundleError::MissingPayload {
                name: entry.name.clone(),
            })?;
            verify(entry, &received)?;
            bytes.push(received);
        }
        Ok(Bundle {
            manifest,
            payloads: bytes,
        })
    }

    /// The bundle's manifest.
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    /// The bytes of the payload at `index`, if the bundle has one there.
    pub fn payload_bytes(&self, index: usize) -> Option<&[u8]> {
        self.payloads.get(index).map(Vec::as_slice)
    }

    /// The bytes of the entrypoint payload.
    ///
    /// Total: a manifest's entry is in range by construction and by decode, and
    /// the payload vectors are the same length as the manifest's.
    pub fn entry_bytes(&self) -> &[u8] {
        &self.payloads[self.manifest.entry as usize]
    }

    /// The bytes of the payload called `name`, if the bundle has one.
    pub fn payload_by_name(&self, name: &str) -> Option<&[u8]> {
        let index = self
            .manifest
            .payloads
            .iter()
            .position(|entry| entry.name == name)?;
        self.payload_bytes(index)
    }

    /// Writes the bundle to `dir` as a `.klbundle` directory, creating it.
    pub fn write(&self, dir: &Path) -> Result<(), BundleError> {
        let payload_dir = dir.join(PAYLOAD_DIR);
        fs::create_dir_all(&payload_dir).map_err(|source| BundleError::Io {
            path: payload_dir.clone(),
            source,
        })?;
        for (entry, bytes) in self.manifest.payloads.iter().zip(&self.payloads) {
            let path = payload_dir.join(&entry.name);
            fs::write(&path, bytes).map_err(|source| BundleError::Io { path, source })?;
        }
        let manifest_path = dir.join(MANIFEST_FILE);
        fs::write(&manifest_path, self.manifest.to_bytes()).map_err(|source| BundleError::Io {
            path: manifest_path,
            source,
        })
    }

    /// Reads a `.klbundle` directory, verifying every payload against the manifest.
    pub fn read(dir: &Path) -> Result<Bundle, BundleError> {
        let manifest_path = dir.join(MANIFEST_FILE);
        let manifest_bytes = fs::read(&manifest_path).map_err(|source| BundleError::Io {
            path: manifest_path.clone(),
            source,
        })?;
        let manifest = BundleManifest::from_bytes(&manifest_bytes).map_err(|source| {
            BundleError::Manifest {
                path: manifest_path,
                source,
            }
        })?;

        let payload_dir = dir.join(PAYLOAD_DIR);
        let mut payloads = Vec::with_capacity(manifest.payloads.len());
        for entry in &manifest.payloads {
            // The name is a plain file name: the decoder rejected anything else,
            // so this join cannot leave `payload_dir`.
            let path = payload_dir.join(&entry.name);
            let bytes = fs::read(&path).map_err(|source| BundleError::Io { path, source })?;
            verify(entry, &bytes)?;
            payloads.push(bytes);
        }
        Ok(Bundle { manifest, payloads })
    }
}

/// A payload on its way into a bundle, before it has been hashed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedPayload {
    /// The payload's name within the bundle.
    pub name: String,
    /// What the payload is.
    pub kind: crate::bundle::PayloadKind,
    /// The payload's bytes.
    pub bytes: Vec<u8>,
}

/// Checks `bytes` against what `entry` records for them.
///
/// Size is checked before the hash so a truncated payload reports the useful
/// error rather than an opaque digest mismatch.
fn verify(entry: &PayloadEntry, bytes: &[u8]) -> Result<(), BundleError> {
    if bytes.len() as u64 != entry.size {
        return Err(BundleError::SizeMismatch {
            name: entry.name.clone(),
            expected: entry.size,
            found: bytes.len() as u64,
        });
    }
    let found = ContentHash::of(bytes);
    if found != entry.hash {
        return Err(BundleError::HashMismatch {
            name: entry.name.clone(),
            expected: entry.hash,
            found,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bundle::PayloadKind;
    use kira_manifest::{BuildProfile, RunnerId};

    /// A scratch directory that removes itself, so a failing test cannot leave
    /// bundles behind and a later run cannot read a previous run's payloads.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            // Unique per process and per tag: the test binary runs its tests in
            // parallel threads within one process.
            let path = std::env::temp_dir().join(format!("kira-live-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("scratch dir");
            TempDir(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bundle() -> Bundle {
        Bundle::build(
            RunnerId::Desktop,
            BuildProfile::Debug,
            vec![
                NamedPayload {
                    name: "app.kbc".to_owned(),
                    kind: PayloadKind::VmBytecode,
                    bytes: b"KBC1 bytecode".to_vec(),
                },
                NamedPayload {
                    name: "logo.png".to_owned(),
                    kind: PayloadKind::Asset,
                    bytes: b"\x89PNG".to_vec(),
                },
            ],
            0,
        )
    }

    #[test]
    fn build_hashes_every_payload() {
        let bundle = bundle();
        assert_eq!(
            bundle.manifest().payloads[0].hash,
            ContentHash::of(b"KBC1 bytecode")
        );
        assert_eq!(bundle.manifest().payloads[0].size, 13);
        assert_eq!(bundle.entry_bytes(), b"KBC1 bytecode");
    }

    #[test]
    fn bundle_round_trips_through_a_directory() {
        let dir = TempDir::new("round-trip");
        let bundle = bundle();
        bundle.write(dir.path()).expect("write");
        assert_eq!(Bundle::read(dir.path()).expect("read"), bundle);
    }

    #[test]
    fn write_lays_out_the_documented_shape() {
        let dir = TempDir::new("layout");
        bundle().write(dir.path()).expect("write");
        assert!(dir.path().join(MANIFEST_FILE).is_file());
        assert!(dir.path().join(PAYLOAD_DIR).join("app.kbc").is_file());
        assert!(dir.path().join(PAYLOAD_DIR).join("logo.png").is_file());
    }

    /// The check that makes a bundle an artifact boundary rather than a hint: a
    /// payload edited behind the manifest's back is refused, not loaded.
    ///
    /// The corruption is deliberately the *same length* as the original, so the
    /// size check cannot catch it and the hash is what has to.
    #[test]
    fn a_corrupted_payload_is_refused_on_read() {
        let dir = TempDir::new("corrupt");
        bundle().write(dir.path()).expect("write");
        assert_eq!(b"KBC1 bytecodX".len(), b"KBC1 bytecode".len());
        fs::write(
            dir.path().join(PAYLOAD_DIR).join("app.kbc"),
            b"KBC1 bytecodX",
        )
        .expect("corrupt");

        let error = Bundle::read(dir.path()).expect_err("a corrupted payload must not load");
        assert!(
            matches!(error, BundleError::HashMismatch { ref name, .. } if name == "app.kbc"),
            "expected a hash mismatch, got {error:?}"
        );
    }

    #[test]
    fn a_truncated_payload_is_refused_on_read() {
        let dir = TempDir::new("truncated");
        bundle().write(dir.path()).expect("write");
        fs::write(dir.path().join(PAYLOAD_DIR).join("app.kbc"), b"KBC").expect("truncate");

        let error = Bundle::read(dir.path()).expect_err("a truncated payload must not load");
        assert!(
            matches!(
                error,
                BundleError::SizeMismatch {
                    expected: 13,
                    found: 3,
                    ..
                }
            ),
            "expected a size mismatch, got {error:?}"
        );
    }

    #[test]
    fn a_missing_payload_file_is_reported() {
        let dir = TempDir::new("missing");
        bundle().write(dir.path()).expect("write");
        fs::remove_file(dir.path().join(PAYLOAD_DIR).join("logo.png")).expect("remove");

        let error = Bundle::read(dir.path()).expect_err("a missing payload must not load");
        assert!(
            matches!(error, BundleError::Io { .. }),
            "expected an i/o error, got {error:?}"
        );
    }

    #[test]
    fn assemble_verifies_received_payloads() {
        let bundle = bundle();
        let received = vec![Some(b"KBC1 bytecode".to_vec()), Some(b"\x89PNG".to_vec())];
        let assembled =
            Bundle::assemble(bundle.manifest().clone(), received).expect("assembles clean");
        assert_eq!(assembled, bundle);
    }

    #[test]
    fn assemble_refuses_a_payload_that_does_not_match() {
        let bundle = bundle();
        let received = vec![Some(b"KBC1 tampered".to_vec()), Some(b"\x89PNG".to_vec())];
        let error = Bundle::assemble(bundle.manifest().clone(), received)
            .expect_err("a tampered payload must not assemble");
        assert!(
            matches!(error, BundleError::HashMismatch { .. }),
            "expected a hash mismatch, got {error:?}"
        );
    }

    #[test]
    fn assemble_reports_a_payload_that_never_arrived() {
        let bundle = bundle();
        let error = Bundle::assemble(
            bundle.manifest().clone(),
            vec![Some(b"KBC1 bytecode".to_vec())],
        )
        .expect_err("a missing payload must not assemble");
        assert!(
            matches!(error, BundleError::MissingPayload { ref name } if name == "logo.png"),
            "expected a missing payload, got {error:?}"
        );
    }

    #[test]
    fn payload_lookup_by_name_finds_bytes() {
        let bundle = bundle();
        assert_eq!(bundle.payload_by_name("logo.png"), Some(&b"\x89PNG"[..]));
        assert!(bundle.payload_by_name("absent").is_none());
    }
}
