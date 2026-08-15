//! Running manifest-declared autobind across a program's whole package graph,
//! before anything is analyzed.
//!
//! # Why here, and why before analysis
//!
//! A generated binding is Kira source: `@FFI.Extern` declarations that the
//! frontend loads with every other file of the package that owns them. So it
//! has to exist on disk before the module walk runs, or the walk reports every
//! call into that C library as an undefined function — which is what a fresh
//! checkout of a package that gitignores its bindings did.
//!
//! It runs here rather than inside program assembly for one reason: assembly
//! never writes. A language server assembles the same program on every
//! keystroke, and a keystroke must not run a C parser or touch the tree. The
//! build writes the bindings; the editor reads what the last build left, which
//! is the same file.
//!
//! # The whole graph, not just the root
//!
//! A library is declared by the package that owns it. An app importing
//! `KiraUIFoundation` declares no `kiratext`, and the bindings it needs are the
//! ones UI Foundation's own headers produce — into UI Foundation's own source
//! tree, where its own modules are compiled from. So every package in the
//! dependency closure is planned, each anchored at its own root.

use std::path::{Path, PathBuf};

use kira_diagnostics::Diagnostic;
use kira_native_lib_definition::{NativeLibrarySpec, TargetTriple};
use kira_project::{AutobindContext, AutobindPlan, AutobindStatus, NativeLibraryPackage};

/// Why a program's native-library declarations could not even be collected.
///
/// Generation failures are not here: those are diagnostics, because a program
/// that cannot bind one library still has everything else to report.
#[derive(Debug, thiserror::Error)]
pub enum NativeDeclarationError {
    /// A package's `NativeLibs` directory exists but could not be listed.
    #[error("cannot list the package's native-library directory `{path}`: {source}")]
    NativeLibsUnreadable {
        /// The directory that could not be listed.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A `package.kira` was found but could not be read or decoded.
    #[error("cannot read the package declaration: {message}")]
    Manifest {
        /// Why it could not be read or decoded, path included.
        message: String,
    },
}

/// Generates every declared binding the program at `source` depends on.
///
/// Returns the diagnostics the run produced: a failure to bind a library that
/// declared bindings is an error, and adopting a binding the package already
/// ships is a note. A program whose packages declare no `autobind` does no work
/// and returns nothing — in particular it never loads a C parser.
pub fn run(source: &Path, target: &TargetTriple) -> Vec<Diagnostic> {
    let packages = match declaring_packages(source) {
        Ok(packages) => packages,
        // Reaching the manifests is program assembly's job to report, with the
        // path in view. Reporting it a second time here would print it twice.
        Err(_) => return Vec::new(),
    };

    let mut planned: Vec<(NativeLibrarySpec, AutobindPlan)> = Vec::new();
    let mut diagnostics = Vec::new();
    for package in &packages {
        let declarations = match kira_project::declared_libraries(package) {
            Ok(declarations) => declarations,
            Err(error) => {
                diagnostics.push(kira_diagnostic_messages::package_messages::autobind_failed(
                    &package.root.display().to_string(),
                    &error.to_string(),
                ));
                continue;
            }
        };
        for (spec, base_dir) in declarations {
            let context = AutobindContext {
                package_root: package.root.clone(),
                source_root: package.root.join("app"),
                base_dir,
                target: target.clone(),
            };
            match kira_project::autobind::plan(&spec, &context) {
                Ok(Some(plan)) => planned.push((spec, plan)),
                Ok(None) => {}
                Err(error) => {
                    diagnostics.push(kira_diagnostic_messages::package_messages::autobind_failed(
                        spec.name(),
                        &error.to_string(),
                    ))
                }
            }
        }
    }

    let stale = planned
        .iter()
        .any(|(_, plan)| plan.status == AutobindStatus::Stale);
    // The C parser is loaded only when something is actually going to be
    // parsed: on an up-to-date tree — every build after the first — this is the
    // difference between reading a few stamp files and mapping libclang.
    let clang = match stale {
        false => None,
        true => match load_clang() {
            Ok(clang) => Some(clang),
            Err(reason) => {
                for (spec, plan) in &planned {
                    if plan.status == AutobindStatus::Stale {
                        diagnostics.push(
                            kira_diagnostic_messages::package_messages::autobind_failed(
                                spec.name(),
                                &reason,
                            ),
                        );
                    }
                }
                return diagnostics;
            }
        },
    };

    for (spec, plan) in &planned {
        match plan.status {
            AutobindStatus::Current => {}
            AutobindStatus::Adopt => {
                diagnostics.push(
                    kira_diagnostic_messages::package_messages::autobind_adopted(
                        &plan.library,
                        &plan.output.display().to_string(),
                    ),
                );
                if let Err(error) = kira_project::autobind::adopt(plan) {
                    diagnostics.push(kira_diagnostic_messages::package_messages::autobind_failed(
                        &plan.library,
                        &error.to_string(),
                    ));
                }
            }
            AutobindStatus::Stale => {
                let Some(clang) = clang.as_ref() else {
                    continue;
                };
                kira_diagnostics::progress!("binding native library");
                if let Err(error) = kira_project::autobind::generate(plan, spec, clang) {
                    diagnostics.push(kira_diagnostic_messages::package_messages::autobind_failed(
                        &plan.library,
                        &error.to_string(),
                    ));
                }
            }
        }
    }
    diagnostics
}

/// Loads the C parser out of the managed LLVM toolchain.
fn load_clang() -> Result<kira_clang::Clang, String> {
    let installation = kira_toolchain::discover(None).map_err(|error| error.to_string())?;
    kira_clang::Clang::load(&installation.home).map_err(|error| error.to_string())
}

/// Every package whose declarations a build at `source` may draw on: the one
/// owning `source`, then each package it depends on, transitively.
///
/// Each group is anchored at its own root, because a declaration's relative
/// paths are written against the package that made it.
pub fn declaring_packages(
    source: &Path,
) -> Result<Vec<NativeLibraryPackage>, NativeDeclarationError> {
    let (root, inline, allow_thin_ffi_shim) = package_declarations(source)?;
    let mut packages = vec![NativeLibraryPackage {
        manifest_paths: native_lib_manifests(&root)?,
        root: root.clone(),
        inline,
        allow_thin_ffi_shim,
    }];
    // A dependency's declarations are read from its own `package.kira`. A
    // dependency that cannot be resolved is not this function's to report: the
    // frontend already names it, with the span to point at.
    let Ok(graph) = kira_package_manager::resolve(&root) else {
        return Ok(packages);
    };
    // One package may be reached by more than one path through the graph, and
    // reading its declarations twice would look like declaring them twice. The
    // root arrives spelled as the user wrote it and the graph spells every
    // package absolutely, so the comparison is on identity rather than text —
    // otherwise the root package is read twice and its headers parsed twice.
    let mut seen: Vec<PathBuf> = vec![identity(&root)];
    for package in graph.packages {
        // The graph names each package's `app/` directory; a declaration's
        // relative paths are written against the package root above it.
        let Some(package_root) = package.source_dir.parent().map(Path::to_path_buf) else {
            continue;
        };
        if seen.contains(&identity(&package_root)) {
            continue;
        }
        seen.push(identity(&package_root));
        // A dependency whose manifest cannot be read is a real fault: its
        // libraries would silently go undeclared and the failure would surface
        // much later as an undeclared-library error naming an import that is
        // perfectly correct.
        let declared = match kira_project::manifest_for(&package_root) {
            Ok(Some(declared)) => declared,
            // No manifest at all is ordinary — not every source directory in a
            // graph is a package with declarations of its own.
            Ok(None) => continue,
            Err(error) => {
                return Err(NativeDeclarationError::Manifest {
                    message: format!("`{}`: {error}", package_root.display()),
                });
            }
        };
        packages.push(NativeLibraryPackage {
            manifest_paths: native_lib_manifests(&package_root)?,
            root: package_root,
            inline: declared.manifest.native_libraries,
            allow_thin_ffi_shim: declared.manifest.allow_thin_ffi_shim,
        });
    }
    Ok(packages)
}

/// A directory in the one spelling two references to it share.
fn identity(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
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
) -> Result<(PathBuf, Vec<NativeLibrarySpec>, bool), NativeDeclarationError> {
    // A manifest that exists but does not read is a real fault worth naming: a
    // build with foreign imports would otherwise fail later as an undeclared
    // library, blaming the import for an unreadable manifest.
    let located =
        kira_project::manifest_for(source).map_err(|error| NativeDeclarationError::Manifest {
            message: error.to_string(),
        })?;
    let Some(located) = located else {
        let root = source
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        return Ok((root, Vec::new(), false));
    };
    let root = match PathBuf::from(&located.path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    Ok((
        root,
        located.manifest.native_libraries,
        located.manifest.allow_thin_ffi_shim,
    ))
}

/// Every `NativeLibs/*.toml` the package ships, as package-relative paths.
///
/// This is the file-per-library spelling; the other is the inline
/// `nativeLibraries` array read from `package.kira`, and a package may use
/// either or both. A missing directory is no libraries, not an error — a
/// program with foreign imports and no declaration anywhere is caught later as
/// an undeclared-library diagnostic that names the library rather than the
/// directory.
fn native_lib_manifests(package_root: &Path) -> Result<Vec<String>, NativeDeclarationError> {
    let dir = package_root.join("NativeLibs");
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => {
            return Err(NativeDeclarationError::NativeLibsUnreadable {
                path: dir.display().to_string(),
                source,
            });
        }
    };
    let mut manifests = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| NativeDeclarationError::NativeLibsUnreadable {
            path: dir.display().to_string(),
            source,
        })?;
        let path = entry.path();
        if path.extension().and_then(|extension| extension.to_str()) == Some("toml")
            && let Some(name) = path.file_name().and_then(|name| name.to_str())
        {
            manifests.push(format!("NativeLibs/{name}"));
        }
    }
    // Deterministic order so a build is reproducible and a diagnostic is stable.
    manifests.sort();
    Ok(manifests)
}
