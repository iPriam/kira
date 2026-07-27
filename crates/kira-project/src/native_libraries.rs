//! Disk resolution of a package's native-library declarations into a catalog.
//!
//! This is the I/O layer the pure model in `kira-native-lib-definition` leaves
//! open. A package declares its C libraries in two places and both are read
//! here: inline in `package.kira` (`let nativeLibraries = [...]`, already
//! decoded into the manifest) and as `NativeLibs/*.toml` files, parsed with
//! `kira-manifest`. Each declaration's paths resolve against its own base
//! directory — the package root for an inline entry, the TOML's own parent for
//! a file — using `Path::exists` as the existence predicate. The result is one
//! [`NativeLinkResolution`] the build path threads to the backend.

use std::path::{Path, PathBuf};

use kira_core::Interner;
use kira_manifest::{NativeLibParseError, parse_native_lib_manifest};
use kira_native_lib_definition::{
    NativeLibraryError, NativeLibrarySpec, ResolvedNativeLibraries, ResolvedNativeLibrary,
    TargetTriple,
};

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

/// Resolves everything a package declares about its C libraries into one
/// catalog.
///
/// `package_root` is the resolved package directory, `inline` are the
/// declarations read out of `package.kira`
/// ([`kira_manifest::ProjectManifest::native_libraries`]), `manifest_paths` are
/// the `NativeLibs/*.toml` files relative to the package root, and `target` is
/// the selected build target.
///
/// The two sources differ only in where their relative paths are anchored: an
/// inline entry writes paths relative to the package root it was declared in, a
/// TOML file relative to its own parent directory — so a manifest may sit
/// anywhere under the package and still name its archives relative to itself.
/// A library declared in both places is a
/// [`NativeLibraryError::DuplicateLibrary`], the same as declaring it twice in
/// either one.
pub fn resolve_native_libraries(
    package_root: &Path,
    inline: &[NativeLibrarySpec],
    manifest_paths: &[String],
    target: &TargetTriple,
) -> Result<NativeLinkResolution, NativeLibraryResolveError> {
    resolve_native_library_packages(
        &[NativeLibraryPackage {
            root: package_root.to_path_buf(),
            inline: inline.to_vec(),
            manifest_paths: manifest_paths.to_vec(),
        }],
        target,
    )
}

/// One package's native-library declarations, anchored at its own root.
#[derive(Debug, Clone)]
pub struct NativeLibraryPackage {
    /// The package directory its relative paths are written against.
    pub root: PathBuf,
    /// What its `package.kira` declares inline.
    pub inline: Vec<NativeLibrarySpec>,
    /// Its `NativeLibs/*.toml` files, relative to `root`.
    pub manifest_paths: Vec<String>,
}

/// Resolves the declarations of a package **and everything it depends on**.
///
/// A library is declared by the package that owns it, not by every package that
/// uses it: `kira-graphics` declares `sokol`, `vulkan`, and `kira_metal`, and an
/// app importing their symbols declares none of them. So the whole dependency
/// closure contributes, each group's relative paths anchored at its own root.
pub fn resolve_native_library_packages(
    packages: &[NativeLibraryPackage],
    target: &TargetTriple,
) -> Result<NativeLinkResolution, NativeLibraryResolveError> {
    let mut resolved: Vec<ResolvedNativeLibrary> = Vec::new();
    for package in packages {
        // Within one package a library declared twice is still an error, so
        // each group resolves on its own before being merged.
        let mut group = Vec::new();
        resolve_one(package, target, &mut group)?;
        ResolvedNativeLibraries::from_resolved(Interner::new(), group.clone())?;
        for library in group {
            // Across packages the nearest declaration wins. An app and the
            // engine it depends on both declaring `sokol` is the normal case
            // here, not a conflict — the app's own is the one that governs,
            // and the groups arrive nearest-first.
            if resolved
                .iter()
                .any(|already| already.name() == library.name())
            {
                continue;
            }
            resolved.push(library);
        }
    }
    let catalog = ResolvedNativeLibraries::from_resolved(Interner::new(), resolved)?;
    Ok(NativeLinkResolution {
        catalog,
        target: target.clone(),
    })
}

/// Resolves one package's two declaration sources into `resolved`.
fn resolve_one(
    package: &NativeLibraryPackage,
    target: &TargetTriple,
    resolved: &mut Vec<ResolvedNativeLibrary>,
) -> Result<(), NativeLibraryResolveError> {
    let package_root = package.root.as_path();
    let inline = &package.inline;
    let manifest_paths = &package.manifest_paths;
    for spec in inline {
        resolved.push(locate(spec, package_root, target)?);
    }
    for relative in manifest_paths {
        let manifest_path = package_root.join(relative);
        let text = std::fs::read_to_string(&manifest_path).map_err(|error| {
            NativeLibraryResolveError::Unreadable {
                path: manifest_path.display().to_string(),
                message: error.to_string(),
            }
        })?;
        let spec = parse_native_lib_manifest(&text).map_err(|source| {
            NativeLibraryResolveError::Malformed {
                path: manifest_path.display().to_string(),
                source: Box::new(source),
            }
        })?;
        let base_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
        resolved.push(locate(&spec, base_dir, target)?);
    }
    Ok(())
}

/// Locates one declaration's files against `base_dir`, reading the disk.
///
/// Only `target`'s archive has to be there: a library declaring every platform
/// it supports is normal, and a checkout has archives for the one it builds.
fn locate(
    spec: &NativeLibrarySpec,
    base_dir: &Path,
    target: &TargetTriple,
) -> Result<ResolvedNativeLibrary, NativeLibraryError> {
    spec.resolve(base_dir, Some(target), |candidate| candidate.exists())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(PathBuf);

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
            resolve_native_libraries(root, &[], &["NativeLibs/ffimath.toml".to_owned()], &host())
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
                .expect("host archive")
                .artifact(),
            Some(root.join("NativeLibs/lib/libffimath-macos.a").as_path()),
        );
        assert_eq!(
            resolution
                .catalog
                .resolve_import(symbol, &wasm())
                .expect("wasm archive")
                .artifact(),
            Some(root.join("NativeLibs/lib/libffimath-wasm.a").as_path()),
        );
    }

    #[test]
    fn an_inline_declaration_resolves_against_the_package_root() {
        // The blocker this whole path exists for: a package that declares its
        // libraries in `package.kira` and ships no `NativeLibs/*.toml` at all.
        let dir = TempDir::new("inline");
        let root = dir.path();
        write(&root.join("generated/native/aarch64-macos/libsokol.a"), "");
        let manifest = kira_manifest::load_declaration(
            r#"
Package Demo {
    let nativeLibraries = [
        NativeLibrary {
            name: "sokol",
            linkMode: LinkMode.Static,
            nativeTargets: [
                NativeTarget { triple: "aarch64-macos-none", staticLib: "generated/native/aarch64-macos/libsokol.a", frameworks: ["AppKit"] }
            ],
        }
    ]
}
"#,
        )
        .expect("a readable manifest");

        let mut resolution =
            resolve_native_libraries(root, &manifest.native_libraries, &[], &host())
                .expect("resolution succeeds");
        let symbol = resolution
            .catalog
            .intern_library("sokol")
            .expect("interned");
        let row = resolution
            .catalog
            .resolve_import(symbol, &host())
            .expect("the host row");
        assert_eq!(
            row.artifact(),
            Some(
                root.join("generated/native/aarch64-macos/libsokol.a")
                    .as_path()
            ),
        );
        assert_eq!(row.attributes().frameworks, ["AppKit"]);
    }

    #[test]
    fn a_library_declared_inline_and_in_a_toml_is_a_typed_error() {
        let dir = TempDir::new("bothsources");
        let root = dir.path();
        write(&root.join("NativeLibs/ffimath.toml"), FFIMATH_TOML);
        write(&root.join("NativeLibs/lib/libffimath-macos.a"), "");
        write(&root.join("NativeLibs/lib/libffimath-wasm.a"), "");
        let manifest = kira_manifest::load_declaration(
            r#"
Package Demo {
    let nativeLibraries = [
        NativeLibrary {
            name: "ffimath",
            linkMode: LinkMode.Dynamic,
            nativeTargets: [NativeTarget { triple: "aarch64-macos-none", dynamicLib: "" }],
        }
    ]
}
"#,
        )
        .expect("a readable manifest");

        let error = resolve_native_libraries(
            root,
            &manifest.native_libraries,
            &["NativeLibs/ffimath.toml".to_owned()],
            &host(),
        )
        .expect_err("one library from two sources is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Model(NativeLibraryError::DuplicateLibrary { .. })
        ));
    }

    #[test]
    fn the_targets_own_missing_archive_is_a_typed_error() {
        let dir = TempDir::new("missing-host");
        let root = dir.path();
        write(&root.join("NativeLibs/ffimath.toml"), FFIMATH_TOML);
        // The wasm archive exists; the host one — the target being built — does
        // not, which is the case that must still fail.
        write(&root.join("NativeLibs/lib/libffimath-wasm.a"), "");

        let error =
            resolve_native_libraries(root, &[], &["NativeLibs/ffimath.toml".to_owned()], &host())
                .expect_err("a missing archive for the target is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Model(NativeLibraryError::MissingArchive { .. })
        ));
    }

    #[test]
    fn another_targets_missing_archive_does_not_block_this_build() {
        // A cross-platform library declares every platform it supports, and a
        // checkout has archives for the one it builds. `kira-graphics` declares
        // ten targets and ships three; requiring all of them made it unusable
        // on every machine.
        let dir = TempDir::new("missing-other");
        let root = dir.path();
        write(&root.join("NativeLibs/ffimath.toml"), FFIMATH_TOML);
        write(&root.join("NativeLibs/lib/libffimath-macos.a"), "");

        let resolution =
            resolve_native_libraries(root, &[], &["NativeLibs/ffimath.toml".to_owned()], &host())
                .expect("the host archive is all this build needs");
        assert_eq!(resolution.target, host());
        assert_eq!(resolution.catalog.len(), 1);
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
            &[],
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
        let error = resolve_native_libraries(
            dir.path(),
            &[],
            &["NativeLibs/absent.toml".to_owned()],
            &host(),
        )
        .expect_err("a missing manifest file is rejected");
        assert!(matches!(
            error,
            NativeLibraryResolveError::Unreadable { .. }
        ));
    }
}
