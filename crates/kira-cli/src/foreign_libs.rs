//! Resolving a program's `@FFI.Extern` imports to the C link inputs that
//! satisfy them, for the target this build is for.
//!
//! Every backend needs the same two answers: which target is being built, and
//! what each foreign import's library puts on the link line for it. This module
//! produces both from the package's declarations — inline `nativeLibraries` in
//! `package.kira` *and* any `NativeLibs/*.toml` it ships — plus the program's
//! foreign table, so the LLVM link, the hybrid dylib, the VM sidecar, and the
//! wasm `emcc` link all select one way.
//!
//! An archive is not the whole answer: a selected row may also carry Apple
//! frameworks, system libraries, and linker flags, and may carry those and no
//! archive at all. So what comes back is a [`NativeLinkInputs`], not a path
//! list.
//!
//! Selection is exact and structured: a host-only library asked for on wasm is a
//! clean structural miss, named as such before any code generation.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;
use kira_native_lib_definition::{
    ImportResolveError, NativeLibrarySpec, NativeLinkInputs, TargetTriple,
};
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
    /// The package's `package.kira` was found but could not be read or decoded.
    #[error("cannot read the package declaration: {message}")]
    Manifest {
        /// Why it could not be read or decoded, path included.
        message: String,
    },
    /// An import named a library the package does not declare for this target.
    #[error(
        "foreign import names native library `{library}`, which this package does not declare \
         for target `{target}`\n\
         note: declare it in `package.kira` as a `NativeLibrary` entry with a `NativeTarget` \
         whose `triple` is `{target}`, or add `NativeLibs/{library}.toml` with a matching \
         `[target.{target}]` section"
    )]
    UndeclaredLibrary {
        /// The undeclared library name.
        library: String,
        /// The target this build selected.
        target: TargetTriple,
    },
    /// A declared library has no row for the selected target.
    #[error(
        "native library `{library}` has no native artifact for target `{target}`\n\
         note: this is the host-only-library-on-wasm case; add a target row for \
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

/// Resolves `program`'s foreign imports to link inputs for `target`.
///
/// Returns `None` when the program declares no foreign imports — the common case
/// that keeps a build with no FFI from reading a manifest or a `NativeLibs`
/// directory that need not exist. Otherwise every distinct library an import
/// names is resolved to its row for `target`, and the rows are gathered into one
/// [`NativeLinkInputs`] in first-use order.
pub fn resolve(
    source: &Path,
    program: &IrProgram,
    target: TargetTriple,
) -> Result<Option<NativeLinkInputs>, ForeignResolveError> {
    if program.foreign_imports.is_empty() {
        return Ok(None);
    }

    // Every package in the dependency closure contributes: a library is
    // declared by the package that owns it, not by every package that uses it.
    // `kira-graphics` declares `sokol`, `vulkan`, and `kira_metal`; an app
    // importing their symbols declares none of them.
    let packages = declaring_packages(source)?;
    let resolution = kira_project::resolve_native_library_packages(&packages, &target)?;
    let mut catalog = resolution.catalog;

    let mut inputs = NativeLinkInputs::default();
    for (index, entry) in program.foreign_imports.iter().enumerate() {
        let library = entry.import.library();
        let symbol = catalog.intern_library(library).map_err(|_| {
            ForeignResolveError::NameSpaceExhausted {
                library: library.to_owned(),
            }
        })?;
        let row = catalog
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
            })?;
        // A library the runtime opens, or one this platform does not have,
        // contributes nothing to the link line.
        match row {
            Some(row) => inputs.push_row(row),
            None => inputs.mark_unavailable(index),
        }
    }

    Ok(Some(inputs))
}

/// Every package whose declarations this build may draw on: the one owning
/// `source`, then each package it depends on, transitively.
///
/// Each group is anchored at its own root, because a declaration's relative
/// paths are written against the package that made it.
fn declaring_packages(
    source: &Path,
) -> Result<Vec<kira_project::NativeLibraryPackage>, ForeignResolveError> {
    let (root, inline) = package_declarations(source)?;
    let mut packages = vec![kira_project::NativeLibraryPackage {
        manifest_paths: native_lib_manifests(&root)?,
        root: root.clone(),
        inline,
    }];
    // A dependency's declarations are read from its own `package.kira`. A
    // dependency that cannot be resolved is not this function's to report: the
    // frontend already names it, with the span to point at.
    let Ok(graph) = kira_package_manager::resolve(&root) else {
        return Ok(packages);
    };
    // One package may be reached by more than one path through the graph, and
    // reading its declarations twice would look like declaring them twice.
    let mut seen: Vec<PathBuf> = vec![root.clone()];
    for package in graph.packages {
        // The graph names each package's `app/` directory; a declaration's
        // relative paths are written against the package root above it.
        let Some(package_root) = package.source_dir.parent().map(Path::to_path_buf) else {
            continue;
        };
        if seen.contains(&package_root) {
            continue;
        }
        seen.push(package_root.clone());
        let Ok(Some(declared)) = kira_project::manifest_for(&package_root) else {
            continue;
        };
        packages.push(kira_project::NativeLibraryPackage {
            manifest_paths: native_lib_manifests(&package_root)?,
            root: package_root,
            inline: declared.manifest.native_libraries,
        });
    }
    Ok(packages)
}

/// The package directory `source` belongs to and the libraries it declares
/// inline.
///
/// A declaration's paths are resolved relative to the package that owns the
/// build, which is the directory of the `package.kira` above the source; a bare
/// `.kira` file with no package uses its own directory and declares nothing
/// inline.
fn package_declarations(
    source: &Path,
) -> Result<(PathBuf, Vec<NativeLibrarySpec>), ForeignResolveError> {
    // A manifest that exists but does not read is a real fault worth naming: a
    // build with foreign imports would otherwise fail later as an undeclared
    // library, blaming the import for an unreadable manifest.
    let located =
        kira_project::manifest_for(source).map_err(|error| ForeignResolveError::Manifest {
            message: error.to_string(),
        })?;
    let Some(located) = located else {
        let root = source
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok((root, Vec::new()));
    };
    let root = match PathBuf::from(&located.path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((root, located.manifest.native_libraries))
}

/// Every `NativeLibs/*.toml` the package ships, as package-relative paths.
///
/// This is the file-per-library spelling; the other is the inline
/// `nativeLibraries` array read from `package.kira`, and a package may use
/// either or both. A missing directory is no libraries, not an error — a
/// program with foreign imports and no declaration anywhere is caught later as
/// an undeclared-library diagnostic that names the library rather than the
/// directory.
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
