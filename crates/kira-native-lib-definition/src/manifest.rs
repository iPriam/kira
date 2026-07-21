//! The unresolved native-library manifest and its per-target resolution.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::triple::TargetTriple;

/// One target row of a native-library manifest before resolution.
///
/// `static_lib` is a path to the archive, relative to the manifest's own TOML
/// file. It is resolved against a base directory in [`NativeLibraryManifest::resolve`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTargetRow {
    triple: TargetTriple,
    static_lib: String,
}

impl NativeTargetRow {
    /// Builds a target row from its triple and manifest-relative archive path.
    pub fn new(triple: TargetTriple, static_lib: impl Into<String>) -> Self {
        Self {
            triple,
            static_lib: static_lib.into(),
        }
    }

    /// The target this row provides an archive for.
    pub fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// The manifest-relative path to the static archive.
    pub fn static_lib(&self) -> &str {
        &self.static_lib
    }
}

/// A native-library manifest as declared, before its archives are located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLibraryManifest {
    name: String,
    targets: Vec<NativeTargetRow>,
}

impl NativeLibraryManifest {
    /// Builds and validates a manifest.
    ///
    /// Rejects a row whose archive path is empty ([`NativeLibraryError::PathlessRow`])
    /// and two rows naming the same target ([`NativeLibraryError::DuplicateTarget`]).
    pub fn new(
        name: impl Into<String>,
        targets: Vec<NativeTargetRow>,
    ) -> Result<Self, NativeLibraryError> {
        let name = name.into();
        let mut seen = HashSet::with_capacity(targets.len());
        for row in &targets {
            if row.static_lib.trim().is_empty() {
                return Err(NativeLibraryError::PathlessRow {
                    library: name.clone(),
                    triple: row.triple.clone(),
                });
            }
            if !seen.insert(row.triple.clone()) {
                return Err(NativeLibraryError::DuplicateTarget {
                    library: name.clone(),
                    triple: row.triple.clone(),
                });
            }
        }
        Ok(Self { name, targets })
    }

    /// The library name (the key a foreign import resolves against).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared target rows.
    pub fn targets(&self) -> &[NativeTargetRow] {
        &self.targets
    }

    /// Locates each archive relative to `base_dir`, without touching the disk.
    ///
    /// Joins `base_dir` with each row's relative `static_lib` into an absolute
    /// path (a pure join, no I/O), then asks the injected `exists` predicate
    /// whether the archive is present. A row whose archive is absent is a
    /// [`NativeLibraryError::MissingArchive`]. The predicate is the seam the I/O
    /// layer fills with `|path| path.exists()`.
    pub fn resolve(
        &self,
        base_dir: &Path,
        exists: impl Fn(&Path) -> bool,
    ) -> Result<ResolvedNativeLibrary, NativeLibraryError> {
        let mut rows = Vec::with_capacity(self.targets.len());
        for row in &self.targets {
            let archive = base_dir.join(&row.static_lib);
            if !exists(&archive) {
                return Err(NativeLibraryError::MissingArchive {
                    library: self.name.clone(),
                    triple: row.triple.clone(),
                    path: archive,
                });
            }
            rows.push(ResolvedTargetRow {
                triple: row.triple.clone(),
                archive,
            });
        }
        Ok(ResolvedNativeLibrary {
            name: self.name.clone(),
            targets: rows,
        })
    }
}

/// One target row whose archive path has been located on a concrete base directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTargetRow {
    triple: TargetTriple,
    archive: PathBuf,
}

impl ResolvedTargetRow {
    /// The target this row provides an archive for.
    pub fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// The located archive path.
    pub fn archive(&self) -> &Path {
        &self.archive
    }
}

/// A native library whose per-target archives have all been located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNativeLibrary {
    name: String,
    targets: Vec<ResolvedTargetRow>,
}

impl ResolvedNativeLibrary {
    /// Builds a resolved library from its name and located rows.
    pub fn new(name: impl Into<String>, targets: Vec<ResolvedTargetRow>) -> Self {
        Self {
            name: name.into(),
            targets,
        }
    }

    /// The library name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The located target rows.
    pub fn targets(&self) -> &[ResolvedTargetRow] {
        &self.targets
    }
}

/// Why a native-library manifest could not be validated, resolved, or cataloged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeLibraryError {
    /// A target row named no archive path.
    #[error("native library `{library}` target `{triple}` has no static archive path")]
    PathlessRow {
        /// The library whose row is pathless.
        library: String,
        /// The target of the pathless row.
        triple: TargetTriple,
    },
    /// Two rows named the same target.
    #[error("native library `{library}` declares target `{triple}` more than once")]
    DuplicateTarget {
        /// The library with the duplicate target.
        library: String,
        /// The repeated target.
        triple: TargetTriple,
    },
    /// A row's archive was not present where it resolved to.
    #[error("native library `{library}` target `{triple}` is missing its archive at `{}`", path.display())]
    MissingArchive {
        /// The library whose archive is missing.
        library: String,
        /// The target whose archive is missing.
        triple: TargetTriple,
        /// Where the archive was expected.
        path: PathBuf,
    },
    /// Two libraries in one catalog shared a name.
    #[error("native library `{library}` is declared more than once")]
    DuplicateLibrary {
        /// The repeated library name.
        library: String,
    },
    /// The catalog could not intern another distinct library name.
    #[error("too many distinct native-library names to intern")]
    NameSpaceExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    #[test]
    fn rejects_a_pathless_row() {
        let error = NativeLibraryManifest::new(
            "ffimath",
            vec![NativeTargetRow::new(triple("aarch64-macos-none"), "   ")],
        )
        .expect_err("a pathless row is rejected");
        assert_eq!(
            error,
            NativeLibraryError::PathlessRow {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
            }
        );
    }

    #[test]
    fn rejects_a_duplicate_target() {
        let error = NativeLibraryManifest::new(
            "ffimath",
            vec![
                NativeTargetRow::new(triple("aarch64-macos-none"), "lib/a.a"),
                NativeTargetRow::new(triple("aarch64-macos-none"), "lib/b.a"),
            ],
        )
        .expect_err("a duplicate target is rejected");
        assert_eq!(
            error,
            NativeLibraryError::DuplicateTarget {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
            }
        );
    }

    #[test]
    fn resolve_joins_relative_to_the_base_dir() {
        let manifest = NativeLibraryManifest::new(
            "ffimath",
            vec![NativeTargetRow::new(
                triple("aarch64-macos-none"),
                "lib/libffimath-macos.a",
            )],
        )
        .expect("a valid manifest");
        let base = Path::new("/pkg/NativeLibs");
        let resolved = manifest
            .resolve(base, |_| true)
            .expect("resolution with a satisfying predicate");
        assert_eq!(
            resolved.targets()[0].archive(),
            Path::new("/pkg/NativeLibs/lib/libffimath-macos.a")
        );
    }

    #[test]
    fn resolve_reports_a_missing_archive() {
        let manifest = NativeLibraryManifest::new(
            "ffimath",
            vec![NativeTargetRow::new(
                triple("aarch64-macos-none"),
                "lib/absent.a",
            )],
        )
        .expect("a valid manifest");
        let error = manifest
            .resolve(Path::new("/pkg/NativeLibs"), |_| false)
            .expect_err("a missing archive is rejected");
        assert_eq!(
            error,
            NativeLibraryError::MissingArchive {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
                path: PathBuf::from("/pkg/NativeLibs/lib/absent.a"),
            }
        );
    }
}
