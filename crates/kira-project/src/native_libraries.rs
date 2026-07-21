//! Disk resolution of a package's native-library manifests into a catalog.
//!
//! This is the I/O layer the pure model in `kira-native-lib-definition` leaves
//! open: it reads each `NativeLibs/*.toml` a manifest lists, parses it with
//! `kira-manifest`, and resolves each archive against the TOML's own directory
//! using `Path::exists` as the existence predicate. The result is a
//! [`NativeLinkResolution`] the build path threads to the backend.

use std::path::Path;

use kira_core::Interner;
use kira_manifest::{NativeLibParseError, parse_native_lib_manifest};
use kira_native_lib_definition::{NativeLibraryError, ResolvedNativeLibraries, TargetTriple};

/// A resolved native-library catalog together with the target it was resolved
/// for, ready to hand to a code-generation backend.
#[derive(Debug, Clone)]
pub struct NativeLinkResolution {
    /// The resolved catalog, keyed by interned library name.
    pub catalog: ResolvedNativeLibraries,
    /// The target selected for this build.
    pub target: TargetTriple,
}

/// Why a package's native libraries could not be resolved from disk.
#[derive(Debug, thiserror::Error)]
pub enum NativeLibraryResolveError {
    /// A listed `NativeLibs/*.toml` could not be read.
    #[error("cannot read native-library manifest `{path}`: {message}")]
    Unreadable {
        /// The manifest that could not be read.
        path: String,
        /// The underlying I/O failure, rendered.
        message: String,
    },
    /// A listed manifest was found but could not be parsed.
    ///
    /// The parse error is boxed: `toml::de::Error` is large, and an unboxed
    /// variant would bloat every `Result` this function returns.
    #[error("cannot parse native-library manifest `{path}`: {source}")]
    Malformed {
        /// The manifest that could not be parsed.
        path: String,
        /// Why parsing failed.
        #[source]
        source: Box<NativeLibParseError>,
    },
    /// A resolved manifest or the catalog itself was invalid (missing archive,
    /// duplicate library).
    #[error(transparent)]
    Model(#[from] NativeLibraryError),
}

/// Resolves every native-library manifest a package lists into one catalog.
///
/// `package_root` is the resolved package directory, `native_libraries` are the
/// `NativeLibs/*.toml` paths relative to it (from
/// [`kira_manifest::ProjectManifest::native_libraries`]), and `target` is the
/// selected build target. Each manifest's archives resolve relative to that
/// TOML file's own parent directory, so a manifest may sit anywhere under the
/// package and still name its archives relative to itself.
pub fn resolve_native_libraries(
    package_root: &Path,
    native_libraries: &[String],
    target: &TargetTriple,
) -> Result<NativeLinkResolution, NativeLibraryResolveError> {
    let mut resolved = Vec::with_capacity(native_libraries.len());
    for relative in native_libraries {
        let manifest_path = package_root.join(relative);
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            NativeLibraryResolveError::Unreadable {
                path: manifest_path.display().to_string(),
                message: error.to_string(),
            }
        })?;
        let manifest = parse_native_lib_manifest(&text).map_err(|source| {
            NativeLibraryResolveError::Malformed {
                path: manifest_path.display().to_string(),
                source: Box::new(source),
            }
        })?;
        let base_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        resolved.push(manifest.resolve(base_dir, |candidate| candidate.exists())?);
    }
    let catalog = ResolvedNativeLibraries::from_resolved(Interner::new(), resolved)?;
    Ok(NativeLinkResolution {
        catalog,
        target: target.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            let base = std::env::temp_dir().join(format!(
                "kira-native-lib-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            std::fs::create_dir_all(&base).expect("a scratch directory");
            Self(base)
        }

        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn write(path: &Path, contents: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("a parent directory");
        }
        std::fs::write(path, contents).expect("write a fixture file");
    }

    fn host() -> TargetTriple {
        TargetTriple::parse("aarch64-macos-none").expect("a valid host triple")
    }

    fn wasm() -> TargetTriple {
        TargetTriple::parse("wasm32-emscripten-unknown").expect("a valid wasm triple")
    }

    const FFIMATH_TOML: &str = r#"
name = "ffimath"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffimath-macos.a"
[[target]]
triple = "wasm32-emscripten-unknown"
staticLib = "lib/libffimath-wasm.a"
"#;

    #[test]
    fn host_and_wasm_rows_resolve_relative_to_their_own_toml() {
        let dir = TempDir::new("resolve");
        let root = dir.path();
        write(&root.join("NativeLibs/ffimath.toml"), FFIMATH_TOML);
        write(&root.join("NativeLibs/lib/libffimath-macos.a"), "");
        write(&root.join("NativeLibs/lib/libffimath-wasm.a"), "");

        let mut resolution =
            resolve_native_libraries(root, &["NativeLibs/ffimath.toml".to_owned()], &host())
                .expect("resolution succeeds");
        assert_eq!(resolution.catalog.len(), 1);
        let symbol = resolution
            .catalog
            .intern_library("ffimath")
            .expect("interned");
        assert_eq!(
            resolution
                .catalog
                .resolve_import(symbol, &host())
                .expect("host archive"),
            root.join("NativeLibs/lib/libffimath-macos.a"),
        );
        assert_eq!(
            resolution
                .catalog
                .resolve_import(symbol, &wasm())
                .expect("wasm archive"),
            root.join("NativeLibs/lib/libffimath-wasm.a"),
        );
    }

    #[test]
    fn a_missing_archive_is_a_typed_error() {
        let dir = TempDir::new("missing");
        let root = dir.path();
        write(&root.join("NativeLibs/ffimath.toml"), FFIMATH_TOML);
        // Only the host archive exists; the wasm one is absent.
        write(&root.join("NativeLibs/lib/libffimath-macos.a"), "");

        let error =
            resolve_native_libraries(root, &["NativeLibs/ffimath.toml".to_owned()], &host())
                .expect_err("a missing archive is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Model(NativeLibraryError::MissingArchive { .. })
        ));
    }

    #[test]
    fn a_duplicate_library_across_manifests_is_a_typed_error() {
        let dir = TempDir::new("dup");
        let root = dir.path();
        write(&root.join("NativeLibs/a.toml"), FFIMATH_TOML);
        write(&root.join("NativeLibs/b.toml"), FFIMATH_TOML);
        write(&root.join("NativeLibs/lib/libffimath-macos.a"), "");
        write(&root.join("NativeLibs/lib/libffimath-wasm.a"), "");

        let error = resolve_native_libraries(
            root,
            &[
                "NativeLibs/a.toml".to_owned(),
                "NativeLibs/b.toml".to_owned(),
            ],
            &host(),
        )
        .expect_err("a duplicate library name is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Model(NativeLibraryError::DuplicateLibrary { .. })
        ));
    }

    #[test]
    fn an_unreadable_manifest_is_a_typed_error() {
        let dir = TempDir::new("unreadable");
        let error =
            resolve_native_libraries(dir.path(), &["NativeLibs/absent.toml".to_owned()], &host())
                .expect_err("a missing manifest file is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Unreadable { .. }
        ));
    }
}
