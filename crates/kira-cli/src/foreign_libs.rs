//! Resolving a program's `@FFI.Extern` imports to the C link inputs that
//! satisfy them, for the target this build is for.
//!
//! Every backend needs the same two answers: which target is being built, and
//! what each foreign import's library puts on the link line for it. This module
//! produces both from the package's declarations — inline `nativeLibraries` in
//! `package.kira` *and* any `NativeLibs/*.toml` it ships — plus the program's
//! foreign table, so the LLVM link, the hybrid dylib, the VM direct-binding
//! payload, and the wasm `emcc` link all select one way.
//!
//! An archive is not the whole answer: a selected row may also carry Apple
//! frameworks, system libraries, and linker flags, and may carry those and no
//! archive at all. So what comes back is a [`NativeLinkInputs`], not a path
//! list.
//!
//! Selection is exact and structured: a host-only library asked for on wasm is a
//! clean structural miss, named as such before any code generation.

use std::path::Path;

use kira_ir::IrProgram;
use kira_native_lib_definition::{ImportResolveError, NativeLinkInputs, TargetTriple};
use kira_project::NativeLibraryResolveError;

use crate::options::Device;

/// Why a program's foreign imports could not be resolved to archives.
#[derive(Debug, thiserror::Error)]
pub enum ForeignResolveError {
    /// A `NativeLibs/*.toml` could not be read, parsed, or resolved.
    #[error(transparent)]
    Resolve(#[from] NativeLibraryResolveError),
    /// The packages whose declarations this build draws on could not be listed.
    #[error(transparent)]
    Declarations(#[from] kira_build::NativeDeclarationError),
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

/// The structured target triple a `--device` or `--target` selects.
///
/// Host builds resolve against this machine's `arch-os-abi`; a Web device
/// resolves against the emscripten wasm triple a package's wasm rows name; and
/// a cross target *is* a triple already, which is the whole reason `--target`
/// takes the manifest's own spelling. A package declaring
/// `[target.aarch64-linux-gnu]` therefore has its archives selected by a cross
/// build without anything here knowing that cross builds exist.
pub fn target_for_device(device: &Device) -> TargetTriple {
    match device {
        Device::Host => host_triple(),
        Device::Cross(target) => target.triple().clone(),
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
    let packages = kira_build::declaring_packages(source)?;
    let resolution = kira_project::resolve_native_library_packages(&packages, &target)?;
    let mut catalog = resolution.catalog;

    let mut inputs = NativeLinkInputs::default();
    for (index, entry) in program.foreign_imports.iter().enumerate() {
        // A system call names the kernel, which no package declares and no link
        // line mentions. Resolving it would ask the catalog for a library called
        // `""` and report it undeclared, sending the author to write a
        // `nativeLibraries` row that cannot exist.
        if !entry.import.abi().binds_a_library_symbol() {
            continue;
        }
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
        // contributes nothing to the link line — but only the second means the
        // symbol is missing, so only the second makes its adapter trap.
        match row {
            Some(row) => {
                let path = catalog
                    .foreign_library_path(symbol, &target)
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
                    .ok_or_else(|| ForeignResolveError::NoArtifactForTarget {
                        library: library.to_owned(),
                        target: target.clone(),
                    })?;
                inputs.push_library(library, path, row);
            }
            None if catalog.is_excluded(symbol, &target) => inputs.mark_unavailable(index),
            // An *optional* library whose row names no artifact is a driver the
            // runtime opens, and its symbols stay undefined in whatever links
            // the adapters. Mach-O and ELF bind those when the library loads;
            // PE has no such step, so on Windows the same declaration is a link
            // error naming every entry point at once. Trapping by name instead
            // says which call was not available, and leaves the program able to
            // reach the driver through explicit symbol lookup — the only way
            // that works there anyway.
            //
            // Optional is what separates a driver from `kira_runtime`, which is
            // declared the same way and whose symbols are already on every link
            // line because they live in the runtime archive. Treating that one
            // as unavailable trapped the dynamic-FFI surface itself.
            None if !target.resolves_symbols_at_load() && catalog.is_optional(symbol) => {
                inputs.mark_unavailable(index)
            }
            None => {
                if let Some(path) =
                    catalog
                        .foreign_library_path(symbol, &target)
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
                {
                    inputs.push_library_path(library, path);
                }
            }
        }
    }

    Ok(Some(inputs))
}
