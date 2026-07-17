//! The runner's cache: getting a bundle onto disk without breaking what is
//! already running.
//!
//! A bundle is written to disk rather than kept in memory because a hybrid
//! bundle's native half is a dynamic library, and `dlopen` takes a path: the OS
//! loader is the one consumer that cannot be handed bytes.
//!
//! Two rules make this more than a `write_all`.
//!
//! **Never delete what was not staged here.** The cache is whatever `--cache`
//! named, and a fresh stage clears it — so `--cache ~/Documents` would erase it.
//! A recursive delete aimed at user input needs a reason to believe the target is
//! its own scratch, and "the flag said so" is not one.
//!
//! **Never rewrite a payload that did not change.** This is what makes a hot
//! patch a hot patch. Rewriting a mapped dylib with byte-identical contents still
//! replaces the inode, and the next `dlopen` maps the new file instead of handing
//! back the image already mapped — so the library "survives" in name only, at a
//! new address, with every pointer into the old one dangling.

use std::fs;
use std::path::Path;

use kira_live::{Bundle, ContentHash, PayloadEntry};

use crate::host::DesktopRunnerError;

/// Empties the runner's cache, refusing to delete anything that is not one.
///
/// The directory is cleared only when it is empty, or when it holds a bundle
/// manifest — the marker that says a previous stage made this directory and it is
/// this runner's to reuse. Anything else fails the session instead of taking
/// somebody's files with it.
pub fn clear_cache(cache: &Path) -> Result<(), DesktopRunnerError> {
    if !cache.exists() {
        return Ok(());
    }
    if !cache.is_dir() {
        return Err(DesktopRunnerError::CacheNotOurs {
            path: cache.to_owned(),
            reason: "it is not a directory",
        });
    }

    let is_empty = fs::read_dir(cache)
        .map_err(|source| DesktopRunnerError::Stage {
            path: cache.to_owned(),
            source,
        })?
        .next()
        .is_none();
    if !is_empty && !cache.join(kira_live::MANIFEST_FILE).is_file() {
        return Err(DesktopRunnerError::CacheNotOurs {
            path: cache.to_owned(),
            reason: "it is not empty and holds no bundle manifest, so it was not staged by a runner",
        });
    }

    fs::remove_dir_all(cache).map_err(|source| DesktopRunnerError::Stage {
        path: cache.to_owned(),
        source,
    })
}

/// Writes `bundle` into `cache`, clearing whatever was there.
///
/// For a first load, where nothing is mapped and nothing is worth keeping.
pub fn stage_fresh(cache: &Path, bundle: &Bundle) -> Result<(), DesktopRunnerError> {
    clear_cache(cache)?;
    bundle.write(cache)?;
    Ok(())
}

/// Writes only the payloads of `bundle` that are not already staged in `cache`.
///
/// A payload whose hash matches what is on disk is left strictly alone — not
/// rewritten with identical bytes, not touched. That is the difference between a
/// hot patch and a slow relaunch.
///
/// A payload already on disk is trusted only if it hashes to what the manifest
/// says. The bundle was verified in memory; the copy on disk could have been
/// touched by anything since.
pub fn restage_changed(cache: &Path, bundle: &Bundle) -> Result<(), DesktopRunnerError> {
    let payload_dir = cache.join(kira_live::PAYLOAD_DIR);
    fs::create_dir_all(&payload_dir).map_err(|source| DesktopRunnerError::Stage {
        path: payload_dir.clone(),
        source,
    })?;

    for (index, entry) in bundle.manifest().payloads.iter().enumerate() {
        // The name is a plain file name — the decoder and the builder both
        // reject anything else — so this join cannot leave the cache.
        let path = payload_dir.join(&entry.name);
        if staged_matches(&path, entry) {
            continue;
        }
        let bytes = bundle
            .payload_bytes(index)
            .ok_or(DesktopRunnerError::NoEntrypoint)?;
        fs::write(&path, bytes).map_err(|source| DesktopRunnerError::Stage { path, source })?;
    }

    // The manifest is small, never mapped, and always rewritten: it is how
    // `clear_cache` recognizes this directory as a runner's own later.
    let manifest_path = cache.join(kira_live::MANIFEST_FILE);
    fs::write(&manifest_path, bundle.manifest().to_bytes()).map_err(|source| {
        DesktopRunnerError::Stage {
            path: manifest_path,
            source,
        }
    })
}

/// Whether the file at `path` is already the payload `entry` describes.
fn staged_matches(path: &Path, entry: &PayloadEntry) -> bool {
    let Ok(bytes) = fs::read(path) else {
        return false;
    };
    bytes.len() as u64 == entry.size && ContentHash::of(&bytes) == entry.hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_live::{NamedPayload, PayloadKind};
    use kira_manifest::{BuildProfile, RunnerId};
    use std::path::PathBuf;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let path =
                std::env::temp_dir().join(format!("kira-stage-{}-{tag}", std::process::id()));
            let _ = fs::remove_dir_all(&path);
            TempDir(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn bundle(bytecode: &[u8], library: &[u8]) -> Bundle {
        Bundle::build(
            RunnerId::Desktop,
            BuildProfile::Debug,
            vec![
                NamedPayload {
                    name: "app.kbc".to_owned(),
                    kind: PayloadKind::VmBytecode,
                    bytes: bytecode.to_vec(),
                },
                NamedPayload {
                    name: "libapp.dylib".to_owned(),
                    kind: PayloadKind::NativeLibrary,
                    bytes: library.to_vec(),
                },
            ],
            0,
        )
        .expect("a valid bundle")
    }

    /// The runner must never delete a directory it did not stage. `--cache` is
    /// user input: without this, pointing it at a real directory erases it.
    #[test]
    fn clearing_refuses_a_directory_it_did_not_stage() {
        let dir = TempDir::new("not-ours");
        fs::create_dir_all(&dir.0).expect("create");
        let precious = dir.0.join("precious.txt");
        fs::write(&precious, b"work that exists nowhere else").expect("write");

        let error = clear_cache(&dir.0).expect_err("somebody's directory must be refused");
        assert!(
            matches!(error, DesktopRunnerError::CacheNotOurs { .. }),
            "got {error:?}"
        );
        assert!(precious.is_file(), "a file that was not ours was deleted");
    }

    #[test]
    fn clearing_an_empty_directory_is_allowed() {
        let dir = TempDir::new("empty");
        fs::create_dir_all(&dir.0).expect("create");
        clear_cache(&dir.0).expect("an empty directory is clearable");
    }

    #[test]
    fn clearing_a_missing_directory_is_not_an_error() {
        let dir = TempDir::new("missing");
        clear_cache(&dir.0).expect("nothing to clear");
    }

    #[test]
    fn clearing_a_previous_stage_is_allowed() {
        let dir = TempDir::new("ours");
        stage_fresh(&dir.0, &bundle(b"KBC1", b"\x7fELF")).expect("stage");
        clear_cache(&dir.0).expect("a runner's own cache is clearable");
        assert!(!dir.0.exists());
    }

    /// The heart of the hot patch: an unchanged payload is not rewritten, so its
    /// inode is stable and a loaded library stays the loaded library.
    #[test]
    fn restaging_leaves_an_unchanged_payload_untouched() {
        let dir = TempDir::new("untouched");
        stage_fresh(&dir.0, &bundle(b"KBC1 before", b"\x7fELF same")).expect("stage");

        let library = dir.0.join(kira_live::PAYLOAD_DIR).join("libapp.dylib");
        let before = fs::metadata(&library)
            .expect("staged")
            .modified()
            .expect("mtime");

        restage_changed(&dir.0, &bundle(b"KBC1 after!", b"\x7fELF same")).expect("restage");

        assert_eq!(
            fs::metadata(&library)
                .expect("still staged")
                .modified()
                .expect("mtime"),
            before,
            "an unchanged payload was rewritten, so its inode moved"
        );
    }

    /// A changed payload is written, or the swap would run the old code.
    #[test]
    fn restaging_writes_a_changed_payload() {
        let dir = TempDir::new("changed");
        stage_fresh(&dir.0, &bundle(b"KBC1 before", b"\x7fELF")).expect("stage");
        restage_changed(&dir.0, &bundle(b"KBC1 after!", b"\x7fELF")).expect("restage");

        let bytecode = dir.0.join(kira_live::PAYLOAD_DIR).join("app.kbc");
        assert_eq!(fs::read(&bytecode).expect("read"), b"KBC1 after!");
    }

    /// A payload on disk is trusted only if it hashes right. Something else
    /// having scribbled on the cache must not survive a restage.
    #[test]
    fn restaging_replaces_a_corrupted_payload() {
        let dir = TempDir::new("corrupt");
        let bundle = bundle(b"KBC1 same", b"\x7fELF");
        stage_fresh(&dir.0, &bundle).expect("stage");

        let bytecode = dir.0.join(kira_live::PAYLOAD_DIR).join("app.kbc");
        fs::write(&bytecode, b"tampered!").expect("tamper");

        restage_changed(&dir.0, &bundle).expect("restage");
        assert_eq!(
            fs::read(&bytecode).expect("read"),
            b"KBC1 same",
            "a tampered payload was left on disk"
        );
    }

    /// Restaging into a directory that does not exist yet works: it is the first
    /// stage, just without the clearing.
    #[test]
    fn restaging_creates_the_cache_if_absent() {
        let dir = TempDir::new("absent");
        restage_changed(&dir.0, &bundle(b"KBC1", b"\x7fELF")).expect("restage");
        assert!(dir.0.join(kira_live::MANIFEST_FILE).is_file());
        assert!(
            dir.0
                .join(kira_live::PAYLOAD_DIR)
                .join("libapp.dylib")
                .is_file()
        );
    }
}
