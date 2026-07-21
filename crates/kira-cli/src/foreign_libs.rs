//! Resolving a program's `@FFI.Extern` imports to the C archives that satisfy
//! them, for the target this build is for.
//!
//! Every backend needs the same two answers: which target is being built, and
//! which static archive each foreign import's library resolves to on it. This
//! module produces both from the package's `NativeLibs/*.toml` manifests and the
//! program's foreign table, so the LLVM link, the hybrid dylib, the VM sidecar,
//! and the wasm `emcc` link all select archives one way.
//!
//! Selection is exact and structured: a host-only library asked for on wasm is a
//! clean structural miss, named as such before any code generation.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;
use kira_native_lib_definition::{ImportResolveError, TargetTriple};
use kira_project::NativeLibraryResolveError;

use crate::options::Device;

/// Why a program's foreign imports could not be resolved to archives.
#[derive(Debug, thiserror::Error)]
pub enum ForeignResolveError {
    /// A `NativeLibs/*.toml` could not be read, parsed, or resolved.
    #[error(transparent)]
    Resolve(#[from] NativeLibraryResolveError),
    /// The package's `NativeLibs` directory exists but could not be listed.
    #[error("cannot list the package's native-library directory `{path}`: {source}")]
    NativeLibsUnreadable {
        /// The directory that could not be listed.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// An import named a library the package does not declare for this target.
    #[error(
        "foreign import names native library `{library}`, which this package does not declare \
         for target `{target}`\n\
         note: add `NativeLibs/{library}.toml` with a `[[target]]` row whose `triple` is \
         `{target}` and a `staticLib` path"
    )]
    UndeclaredLibrary {
        /// The undeclared library name.
        library: String,
        /// The target this build selected.
        target: TargetTriple,
    },
    /// A declared library has no archive for the selected target.
    #[error(
        "native library `{library}` has no native artifact for target `{target}`\n\
         note: this is the host-only-library-on-wasm case; add a `[[target]]` row for \
         `{target}`"
    )]
    NoArtifactForTarget {
        /// The declared library missing an artifact for `target`.
        library: String,
        /// The target with no matching row.
        target: TargetTriple,
    },
    /// The catalog could not intern another distinct library name.
    #[error("too many distinct native-library names to intern (`{library}`)")]
    NameSpaceExhausted {
        /// The library whose name could not be interned.
        library: String,
    },
}

/// The structured target triple a `--device` selects.
///
/// Host builds resolve against this machine's `arch-os-abi`; a Web device
/// resolves against the emscripten wasm triple a package's wasm rows name.
pub fn target_for_device(device: Device) -> TargetTriple {
    match device {
        Device::Host => host_triple(),
        Device::Web(kira_backend_api::WasmDevice::Wasm32) => {
            TargetTriple::new("wasm32", "emscripten", "unknown")
        }
        Device::Web(kira_backend_api::WasmDevice::Wasm64) => {
            TargetTriple::new("wasm64", "emscripten", "unknown")
        }
    }
}

/// This machine's build triple in the `arch-os-abi` spelling a manifest uses.
fn host_triple() -> TargetTriple {
    let (os, abi) = match std::env::consts::OS {
        "macos" => ("macos", "none"),
        "linux" => ("linux", "gnu"),
        "windows" => ("windows", "msvc"),
        other => (other, "none"),
    };
    TargetTriple::new(std::env::consts::ARCH, os, abi)
}

/// Resolves `program`'s foreign imports to archives for `target`.
///
/// Returns `None` when the program declares no foreign imports — the common case
/// that keeps a build with no FFI from touching the filesystem for a `NativeLibs`
/// directory that need not exist. Otherwise every distinct library an import
/// names is resolved to its archive for `target`, in first-use order.
pub fn resolve(
    source: &Path,
    program: &IrProgram,
    target: TargetTriple,
) -> Result<Option<Vec<PathBuf>>, ForeignResolveError> {
    if program.foreign_imports.is_empty() {
        return Ok(None);
    }

    let package_root = package_root_of(source);
    let manifests = native_lib_manifests(&package_root)?;
    let resolution = kira_project::resolve_native_libraries(&package_root, &manifests, &target)?;
    let mut catalog = resolution.catalog;

    let mut archives: Vec<PathBuf> = Vec::new();
    for entry in &program.foreign_imports {
        let library = entry.import.library();
        let symbol = catalog.intern_library(library).map_err(|_| {
            ForeignResolveError::NameSpaceExhausted {
                library: library.to_owned(),
            }
        })?;
        let archive = catalog
            .resolve_import(symbol, &target)
            .map_err(|error| match error {
                ImportResolveError::UndeclaredLibrary { library } => {
                    ForeignResolveError::UndeclaredLibrary {
                        library,
                        target: target.clone(),
                    }
                }
                ImportResolveError::NoArtifactForTarget { library, target } => {
                    ForeignResolveError::NoArtifactForTarget { library, target }
                }
            })?
            .to_path_buf();
        if !archives.contains(&archive) {
            archives.push(archive);
        }
    }

    Ok(Some(archives))
}

/// The package directory `source` belongs to, or its own directory.
///
/// A `NativeLibs/*.toml` is resolved relative to the package that owns the
/// build, which is the directory of the `package.kira` above the source; a bare
/// `.kira` file with no package uses its own directory.
fn package_root_of(source: &Path) -> PathBuf {
    if let Ok(Some(manifest)) = kira_project::manifest_for(source) {
        let manifest_path = PathBuf::from(&manifest.path);
        if let Some(parent) = manifest_path.parent()
            && !parent.as_os_str().is_empty()
        {
            return parent.to_path_buf();
        }
    }
    source
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Every `NativeLibs/*.toml` the package ships, as package-relative paths.
///
/// A package declares its native libraries by shipping their manifests under
/// `NativeLibs/`; each one found there is a declared library. A missing
/// directory is no libraries, not an error — a program with foreign imports and
/// no `NativeLibs` is caught later as an undeclared-library diagnostic that names
/// the library rather than the directory.
fn native_lib_manifests(package_root: &Path) -> Result<Vec<String>, ForeignResolveError> {
    let dir = package_root.join("NativeLibs");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(ForeignResolveError::NativeLibsUnreadable {
                path: dir.display().to_string(),
                source,
            });
        }
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| ForeignResolveError::NativeLibsUnreadable {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("toml")
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            manifests.push(format!("NativeLibs/{name}"));
        }
    }
    // Deterministic order so a build is reproducible and a diagnostic is stable.
    manifests.sort();
    Ok(manifests)
}
