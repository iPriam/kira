//! Source on disk to verified IR: the one pipeline everything compiles through.
//!
//! This used to live in `kira-cli`, and moving it here is what makes the CLI a
//! driver rather than the compiler. The reason is a second consumer, not tidiness:
//! a Rust crate embedding a Kira library builds that library from its own
//! `build.rs`, and a `build.rs` that reimplemented package resolution, module
//! loading, source mapping, or build-kind discovery would drift from `kira` in
//! exactly the ways that make a bug reproduce on one path and not the other.
//!
//! When an entry belongs to a package, this pipeline resolves its transitive path
//! dependencies from `package.kira` before walking imports. A library package
//! contributes every `.kira` file below `app/`, including files no import reaches;
//! bare `.kira` files keep the same bundled-module-only behavior and need no manifest.
//!
//! # What it does not do
//!
//! It does not decide whether the program is acceptable. Diagnostics come back
//! in [`Compiled::diagnostics`] and the IR comes back regardless — lowering is
//! total — because rendering a diagnostic needs a source map and a renderer that
//! only the caller knows how to configure. [`Compiled::has_errors`] is the
//! question a caller asks; what to print, and what exit code to use, stays with
//! whoever owns the terminal.

use std::collections::HashSet;
use std::path::Path;

use kira_diagnostic_messages::diagnostic_code::DiagnosticCode;
use kira_diagnostic_messages::package_messages::{lockfile_sync_failed, lockfile_synced};
use kira_diagnostics::Diagnostic;
use kira_ir::IrProgram;
use kira_semantics::{
    BuildKind, DiagnosticAccumulator, FILE_SOURCE_ID, ModuleSource, SourceProgram,
};
use kira_source::SourceMap;

/// Analyzes and lowers the source program to IR.
///
/// This query lives above the VM's dependency cone deliberately: `kira-ir` sits
/// inside it, and the portable core must stay salsa-free. It depends on the
/// analyzer query, so every lexer, parser, and semantic diagnostic accumulates
/// under it and is gathered with
/// `lowered::accumulated::<DiagnosticAccumulator>`.
///
/// Total, because lowering is: a library lowers to IR with `main: None`, and
/// whether that is acceptable was already decided by the frontend from the
/// package's [`BuildKind`].
#[salsa::tracked(returns(clone))]
fn lowered(db: &dyn salsa::Database, source: SourceProgram) -> IrProgram {
    let program = kira_semantics::analyzed(db, source);
    kira_ir::lower(program)
}

/// A compiled program plus everything needed to report on it.
#[derive(Debug)]
pub struct Compiled {
    /// Every file that took part, indexed so a span renders against its own.
    pub sources: SourceMap,
    /// Package-resolution diagnostics followed by frontend diagnostics in source order.
    pub diagnostics: Vec<Diagnostic>,
    /// The lowered program. Present even when `diagnostics` holds errors.
    pub ir: IrProgram,
    /// What the governing package said this build produces.
    pub build_kind: BuildKind,
    /// The package name from the governing manifest, when there is one.
    ///
    /// `None` for a bare `.kira` file handed to the compiler with no
    /// `package.kira` above it. A library artifact is named after its package,
    /// so this is what a library build reads.
    pub package_name: Option<String>,
    /// The package version from the governing manifest, when there is one.
    ///
    /// The generated wrapper crate takes its version from the library's, so the
    /// two never drift apart in a consumer's lockfile.
    pub package_version: Option<String>,
    /// The governing manifest's default execution mode, when there is one.
    pub default_execution_mode: Option<String>,
    /// The governing manifest's default build target, when there is one.
    pub default_build_target: Option<String>,
}

impl Compiled {
    /// Whether anything the frontend reported would stop a build.
    pub fn has_errors(&self) -> bool {
        kira_diagnostics::has_errors(&self.diagnostics)
    }
}

/// Why a source tree could not be compiled at all.
///
/// Distinct from a program that compiled and has errors: these are failures to
/// *reach* the frontend, and none of them has a span to point at.
#[derive(Debug, thiserror::Error)]
pub enum FrontendError {
    /// A source file selected for compilation could not be read.
    #[error("cannot read `{path}`: {source}")]
    Read {
        /// The path that could not be read.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// More files took part than one source map can hold.
    #[error("{message}")]
    SourceMapFull {
        /// The source map's own account of the limit it hit.
        message: String,
    },
    /// A `package.kira` was found above the source and could not be used.
    #[error(transparent)]
    Discovery(#[from] kira_project::DiscoveryError),
    /// The governing package's dependency graph could not be started.
    #[error(transparent)]
    Resolution(#[from] kira_package_manager::ResolveError),
}

/// Reads and compiles `path` through the salsa frontend and IR lowering.
///
/// Returns `Err` only for problems that prevent compiling at all; compile
/// errors are carried in [`Compiled::diagnostics`], not as an error here.
pub fn compile(path: &Path) -> Result<Compiled, FrontendError> {
    compile_as(path, None)
}

/// Compiles `path`, optionally overriding the build kind its manifest implies.
///
/// `kira test` is the one caller that overrides: a suite is entered through
/// the runner a collector generated rather than through `@Main`, so demanding
/// an application entrypoint would refuse a package whose only purpose is
/// tests. Everything else takes the manifest's word, which is why the override
/// is a parameter here rather than a field on the manifest.
pub fn compile_as(path: &Path, kind: Option<BuildKind>) -> Result<Compiled, FrontendError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| FrontendError::Read {
        path: display.clone(),
        source,
    })?;
    kira_diagnostics::progress!("resolving packages");
    let package = package_of(path)?;
    let (package_roots, mut diagnostics) = resolve_package_roots(package.as_ref())?;

    // Everything the entry file imports, transitively, dependencies first. An
    // import that names no readable file comes back as nothing here and is
    // reported by the frontend, which has the span to point at. Resolved package
    // roots sit between project-local modules and toolchain bundles.
    let bundled_roots = kira_program_graph::bundled::bundled_roots();
    let mut modules =
        kira_program_graph::load_modules_with_packages(path, &text, &bundled_roots, &package_roots);
    // Every `.kira` file under a package's source root is a member of that
    // package — app or library. What a package *produces* does not decide which
    // of its own files belong to it, and an app used to compile its entry file
    // plus that file's imports and nothing else: a program split across
    // `app/main.kira` and `app/area/Thing.kira` reported every function in the
    // sibling as undefined, while the identical library layout compiled.
    //
    // Aggregating adds files to the package, not to each other's scope: imports
    // stay file-scoped, so a sibling's `import Foundation` still does not put
    // `Foundation` in this file's namespace.
    if let Some(found) = package.as_ref()
        && let Some(library_sources) = kira_project::library_sources_for_entry(found, path)?
    {
        aggregate_library_modules(
            path,
            &library_sources,
            &bundled_roots,
            &package_roots,
            &mut modules,
        )?;
    }

    // Enforce the `bind-types/` convention across every loaded source: a
    // `*_types.kira` foreign-binding vocabulary file must sit in a `bind-types/`
    // directory. Reported here, the one place the entry and every loaded module
    // path converge with the diagnostics channel.
    diagnostics.extend(bind_types_placement_diagnostics(path, &modules));

    let build_kind = kind.unwrap_or(match package.as_ref().map(|found| found.kind()) {
        Some(kira_manifest::PackageKind::Library) => BuildKind::Library,
        Some(kira_manifest::PackageKind::App) | None => BuildKind::Application,
    });

    // Shaders are compiled before analysis: expansion runs inside salsa
    // queries, which may not read files, so the paths its call sites name are
    // scanned out and compiled here and handed in as an input.
    // A shader path is written relative to the *package* root, the same way
    // `assets` in a manifest is — `ksl!("Shaders/X.ksl")` in `app/main.kira`
    // names `Shaders/X.ksl` beside `package.kira`, not beside the entry file.
    let shader_root = package
        .as_ref()
        .and_then(|found| Path::new(&found.path).parent())
        .or_else(|| path.parent())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    let mut shader_files: Vec<(kira_source::SourceId, &str)> =
        vec![(FILE_SOURCE_ID, text.as_str())];
    shader_files.extend(modules.iter().enumerate().map(|(index, module)| {
        (
            kira_semantics::module_source_id(index),
            module.text.as_str(),
        )
    }));
    // Shader sources are numbered after the entry file and every module, which
    // is where the `SourceMap` below has room for them.
    let shader_base = u32::try_from(modules.len() + 1).unwrap_or(u32::MAX);
    let (shaders, shader_diagnostics, shader_sources) =
        crate::shader::precompile(&shader_root, &shader_files, shader_base);
    diagnostics.extend(shader_diagnostics);
    drop(shader_files);

    let db = salsa::DatabaseImpl::new();
    let module_paths: Vec<String> = modules.iter().map(|module| module.path.clone()).collect();
    let source = SourceProgram::new(
        &db,
        text,
        display.clone(),
        modules,
        build_kind,
        shaders,
        kira_semantics::host_platform(),
    );

    // The SourceMap mirrors the salsa input file for file and in the same order,
    // so diagnostic spans render against the file they were written in: the
    // entry file at `FILE_SOURCE_ID`, then module `i` at `module_source_id(i)`.
    // It holds each file's text *after macro expansion*, because that is the
    // text the parser saw and the text every span is an offset into. A program
    // that declares no macros gets its own bytes back, so this is the file as
    // written for all but a macro-using program.
    kira_diagnostics::progress!("expanding macros");
    let expansion = kira_semantics::expanded(&db, source);
    let mut sources = SourceMap::new();
    let id = sources
        .insert(display, expansion.entry.clone())
        .map_err(|full| FrontendError::SourceMapFull {
            message: full.to_string(),
        })?;
    debug_assert_eq!(id, FILE_SOURCE_ID);
    for (index, path) in module_paths.into_iter().enumerate() {
        let module_text = expansion.modules.get(index).cloned().unwrap_or_default();
        let id =
            sources
                .insert(path, module_text)
                .map_err(|full| FrontendError::SourceMapFull {
                    message: full.to_string(),
                })?;
        debug_assert_eq!(id, kira_semantics::module_source_id(index));
    }
    // Then the shaders, at the ids their diagnostics were written against, so a
    // KSL error renders with the shader's own text and line.
    for (path, text) in shader_sources {
        sources
            .insert(path, text)
            .map_err(|full| FrontendError::SourceMapFull {
                message: full.to_string(),
            })?;
    }

    kira_diagnostics::progress!("analyzing and lowering");
    let ir = lowered(&db, source);
    diagnostics.extend(
        lowered::accumulated::<DiagnosticAccumulator>(&db, source)
            .into_iter()
            .map(|accumulated| accumulated.0.clone()),
    );

    Ok(Compiled {
        sources,
        diagnostics,
        ir,
        build_kind,
        package_name: package.as_ref().map(|found| found.manifest.name.clone()),
        package_version: package.as_ref().map(|found| found.manifest.version.clone()),
        default_execution_mode: package
            .as_ref()
            .map(|found| found.manifest.execution_mode.clone()),
        default_build_target: package.map(|found| found.manifest.build_target),
    })
}

/// Reports every loaded source whose `*_types.kira` name sits outside a
/// `bind-types/` directory (KPK025).
///
/// The check spans the entry file and every aggregated module — the whole set
/// the frontend will analyze — so a misplaced binding-vocabulary file in any
/// package, a dependency included, is caught.
fn bind_types_placement_diagnostics(entry: &Path, modules: &[ModuleSource]) -> Vec<Diagnostic> {
    std::iter::once(entry)
        .chain(modules.iter().map(|module| Path::new(&module.path)))
        .filter(|path| kira_project::is_misplaced_bind_types_file(path))
        .map(|path| {
            kira_diagnostic_messages::package_messages::misplaced_bind_types_file(
                &path.display().to_string(),
            )
        })
        .collect()
}

/// Adds every unreferenced library source and each source's import closure.
fn aggregate_library_modules(
    entry_path: &Path,
    library_sources: &kira_project::LibrarySources,
    bundled_roots: &[kira_program_graph::bundled::BundledRoot],
    package_roots: &[kira_program_graph::PackageRoot],
    modules: &mut Vec<ModuleSource>,
) -> Result<(), FrontendError> {
    let entry_identity = source_identity(entry_path);
    let mut seen = modules
        .iter()
        .map(|module| source_identity(Path::new(&module.path)))
        .collect::<HashSet<_>>();
    seen.insert(entry_identity.clone());

    for source in library_sources.iter() {
        let identity = source_identity(source.path());
        if identity == entry_identity || seen.contains(&identity) {
            continue;
        }

        let display = source.path().display().to_string();
        let text =
            std::fs::read_to_string(source.path()).map_err(|read_error| FrontendError::Read {
                path: display.clone(),
                source: read_error,
            })?;
        let imported = kira_program_graph::load_modules_with_packages(
            source.path(),
            &text,
            bundled_roots,
            package_roots,
        );
        for module in imported {
            if seen.insert(source_identity(Path::new(&module.path))) {
                modules.push(module);
            }
        }
        if seen.insert(identity) {
            modules.push(ModuleSource {
                module: source.module().to_owned(),
                path: display,
                text,
            });
        }
    }
    Ok(())
}

/// Produces a stable filesystem identity without changing diagnostic path spelling.
fn source_identity(path: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Resolves the dependency package roots and preserves every non-fatal package diagnostic.
fn resolve_package_roots(
    package: Option<&kira_project::Manifest>,
) -> Result<(Vec<kira_program_graph::PackageRoot>, Vec<Diagnostic>), FrontendError> {
    let Some(package) = package else {
        return Ok((Vec::new(), Vec::new()));
    };
    let manifest_path = Path::new(&package.path);
    let root_dir = match manifest_path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let graph = kira_package_manager::resolve(root_dir)?;
    let mut diagnostics = graph.diagnostics;
    sync_drifted_lockfile(root_dir, &graph.lockfile, &graph.packages, &mut diagnostics);
    let roots = graph
        .packages
        .into_iter()
        .map(|package| kira_program_graph::PackageRoot::new(package.name, package.source_dir))
        .collect();
    Ok((roots, diagnostics))
}

/// Rewrites `kira.lock` when the manifests have moved out from under it.
///
/// A lockfile is a record of a resolution, so a build that just performed the
/// resolution is exactly the moment to write it down — the alternative is a
/// warning on every command until someone runs `kira sync` by hand, which is
/// a chore the tool can do itself. Only a lockfile that already exists and
/// drifted is rewritten: a project without one has not asked for one, and
/// creating files a command was not pointed at is a surprise.
///
/// The drift warning resolution raised is replaced with the note saying it was
/// handled, so the reader is told what happened rather than what was wrong.
fn sync_drifted_lockfile(
    root_dir: &Path,
    status: &kira_package_manager::LockfileStatus,
    packages: &[kira_package_manager::ResolvedPackage],
    diagnostics: &mut Vec<Diagnostic>,
) {
    if *status != kira_package_manager::LockfileStatus::Drifted {
        return;
    }
    let path = root_dir.join("kira.lock");
    let display = path.display().to_string();
    match kira_package_manager::sync_lockfile(root_dir, packages) {
        Ok(_) => {
            diagnostics.retain(|diagnostic| {
                diagnostic.code != Some(DiagnosticCode::Kpk024LockfileDrift.as_str())
            });
            diagnostics.push(lockfile_synced(&display));
        }
        Err(error) => diagnostics.push(lockfile_sync_failed(&display, &error.to_string())),
    }
}

/// The manifest governing `source`, if a `package.kira` sits above it.
///
/// The manifest is discovered by walking up from the source file, which is what
/// makes `kira build lib/thing.kira` inside a library package build a library
/// without a flag: the package already said so, and a flag that could disagree
/// with it would be a second source of truth.
///
/// No manifest means an application — a bare `.kira` file is a program. A
/// manifest that exists and cannot be read is an error, because building the
/// wrong kind of artifact silently is worse than not building.
fn package_of(source: &Path) -> Result<Option<kira_project::Manifest>, FrontendError> {
    Ok(kira_project::manifest_for(source)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "kira-build-frontend-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            std::fs::create_dir_all(&base).expect("a scratch directory");
            TempDir(base)
        }

        fn write(&self, name: &str, text: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create fixture directories");
            }
            std::fs::write(&path, text).expect("write a fixture");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_bare_file_compiles_as_an_application() {
        let dir = TempDir::new("bare");
        let path = dir.write("main.kira", "@Main function main() { print(1) return }");
        let compiled = compile(&path).expect("compile");
        assert_eq!(compiled.build_kind, BuildKind::Application);
        assert_eq!(compiled.package_name, None);
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert_eq!(compiled.ir.main, Some(0));
    }

    #[test]
    fn a_library_package_reaches_the_frontend_by_its_manifest() {
        let dir = TempDir::new("lib");
        dir.write(
            "package.kira",
            "Package uifoundation {\n    let version = \"0.1.0\"\n    let kind = .Library\n}\n",
        );
        let path = dir.write("uifoundation.kira", "function f() { return }");
        let compiled = compile(&path).expect("compile");
        assert_eq!(compiled.build_kind, BuildKind::Library);
        assert_eq!(compiled.package_name.as_deref(), Some("uifoundation"));
        // No `@Main`, and no KSEM011: the manifest relaxed it.
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert_eq!(compiled.ir.main, None);
    }

    /// A manifest edited after the lockfile was written leaves the two
    /// disagreeing. Compiling resolves the graph anyway, so it writes the
    /// answer down instead of warning about it on every command from here on.
    #[test]
    fn compiling_rewrites_a_drifted_lockfile() {
        let dir = TempDir::new("lockfile-drift");
        dir.write(
            "package.kira",
            "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
        );
        let stale = "version = 1\n\n[root]\nname = \"Core\"\n\n[[package]]\nname = \"Ghost\"\n";
        dir.write("kira.lock", stale);
        let path = dir.write("app/Core.kira", "function value() -> Int { return 1 }");

        let compiled = compile(&path).expect("compile");

        let lock = std::fs::read_to_string(dir.0.join("kira.lock")).expect("read lockfile");
        assert_ne!(stale, lock, "the stale lockfile should have been rewritten");
        assert!(lock.contains("name = \"Core\""), "{lock}");
        assert!(!lock.contains("Ghost"), "{lock}");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPK026")),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KPK024")),
            "the drift warning is replaced by the synced note: {:?}",
            compiled.diagnostics
        );
    }

    /// A project without a lockfile has not asked for one; compiling must not
    /// create files the command was never pointed at.
    #[test]
    fn compiling_does_not_create_a_missing_lockfile() {
        let dir = TempDir::new("lockfile-absent");
        dir.write(
            "package.kira",
            "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
        );
        let path = dir.write("app/Core.kira", "function value() -> Int { return 1 }");

        compile(&path).expect("compile");

        assert!(!dir.0.join("kira.lock").exists());
    }

    #[test]
    fn a_library_directory_compiles_every_source_under_app() {
        let dir = TempDir::new("aggregate-library");
        dir.write(
            "package.kira",
            "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
        );
        let entry = dir.write("app/Core.kira", "function value() -> Int { return 1 }");
        let broken = dir.write("app/Broken.kira", "function broken(");

        let target = kira_project::resolve_target(&dir.0).expect("resolve library directory");
        assert_eq!(target.source_path.as_deref(), entry.to_str());
        let compiled = compile(Path::new(
            target
                .source_path
                .as_deref()
                .expect("library target compilation entry"),
        ))
        .expect("reach the frontend");

        assert!(compiled.has_errors(), "{:?}", compiled.diagnostics);
        assert!(
            compiled
                .sources
                .iter()
                .any(|source| source.path == broken.display().to_string()),
            "{:?}",
            compiled.sources
        );
        let rendered = compiled
            .diagnostics
            .iter()
            .map(|diagnostic| kira_diagnostics::renderer::render(diagnostic, &compiled.sources))
            .collect::<String>();
        assert!(
            rendered.contains(&broken.display().to_string()),
            "{rendered}"
        );
    }

    #[test]
    fn a_relative_entry_path_resolves_its_boundary_manifest() {
        const CHILD_PROCESS: &str = "KIRA_BUILD_RELATIVE_ENTRY_CHILD";

        if std::env::var_os(CHILD_PROCESS).is_some() {
            let compiled =
                compile(Path::new("app/main.kira")).expect("compile the relative package entry");
            assert_eq!(compiled.build_kind, BuildKind::Application);
            assert_eq!(compiled.package_name.as_deref(), Some("RelativeApp"));
            assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
            return;
        }

        let dir = TempDir::new("relative-entry");
        dir.write(
            "package.kira",
            "Package RelativeApp {\n    let kind = .App\n}\n",
        );
        dir.write("app/main.kira", "@Main function main() { return }");

        let current_thread = std::thread::current();
        let test_name = current_thread.name().expect("the libtest test name");
        let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
            .arg("--exact")
            .arg(test_name)
            .arg("--nocapture")
            .env(CHILD_PROCESS, "1")
            .current_dir(&dir.0)
            .output()
            .expect("run the relative-path test in its package directory");
        assert!(
            output.status.success(),
            "child test failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    #[test]
    fn a_package_compiles_with_resolver_fed_dependency_modules() {
        let dir = TempDir::new("resolved-package");
        dir.write(
            "editor/package.kira",
            r#"Package EditorApp {
    let version = "0.1.0"
    let kind = .App
    let dependencies = [Dependency { name: "Core", path: "../core" }]
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Host }
}
"#,
        );
        dir.write(
            "core/package.kira",
            r#"Package Core {
    let version = "0.1.0"
    let kind = .Library
    let moduleRoot = "Core"
}
"#,
        );
        let values = std::fs::canonicalize(dir.write(
            "core/app/Values.kira",
            "function coreValue() -> Int { return 41 }",
        ))
        .expect("canonical values path");
        let broken = std::fs::canonicalize(dir.write(
            "core/app/Broken.kira",
            "function brokenValue() -> Int { return missingFromCore }",
        ))
        .expect("canonical broken path");
        let entry = dir.write(
            "editor/app/main.kira",
            "import Core\n@Main function main() { print(coreValue() + 1) return }",
        );

        let compiled = compile(&entry).expect("compile a resolved package graph");
        let source_paths = compiled
            .sources
            .iter()
            .map(|source| source.path.as_str())
            .collect::<Vec<_>>();
        assert!(
            source_paths.contains(&values.to_string_lossy().as_ref()),
            "{source_paths:?}"
        );
        assert!(
            source_paths.contains(&broken.to_string_lossy().as_ref()),
            "{source_paths:?}"
        );
        assert!(
            !compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Some("KSEM032")),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == Some("KSEM060") && diagnostic.message.contains("coreValue")
            }),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled
                .diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code.is_some_and(|code| code.starts_with("KPK")) }),
            "{:?}",
            compiled.diagnostics
        );

        let library_diagnostic = compiled
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.code == Some("KSEM060") && diagnostic.message.contains("missingFromCore")
            })
            .expect("the dependency module diagnostic");
        let rendered = kira_diagnostics::renderer::render(library_diagnostic, &compiled.sources);
        assert!(
            rendered.contains(&broken.display().to_string()),
            "{rendered}"
        );
        assert_eq!(compiled.default_execution_mode.as_deref(), Some("llvm"));
        assert_eq!(compiled.default_build_target.as_deref(), Some("host"));
    }

    #[test]
    fn package_resolution_diagnostics_are_returned_with_frontend_diagnostics() {
        let dir = TempDir::new("resolution-diagnostic");
        dir.write(
            "app/package.kira",
            r#"Package BrokenApp {
    let dependencies = [Dependency { name: "Missing", path: "../missing" }]
}
"#,
        );
        let entry = dir.write(
            "app/app/main.kira",
            "import Missing\n@Main function main() { return }",
        );

        let compiled = compile(&entry).expect("resolution remains total below the root");
        let diagnostic = compiled
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Some("KPK020"))
            .expect("the missing dependency package diagnostic");
        assert!(diagnostic.primary_label().is_none());
        let rendered = kira_diagnostics::renderer::render(diagnostic, &compiled.sources);
        assert!(rendered.contains("error[KPK020]"), "{rendered}");
    }

    #[test]
    fn a_missing_file_is_an_error_rather_than_an_empty_program() {
        let error =
            compile(Path::new("/nonexistent/kira-build/x.kira")).expect_err("a missing file");
        assert!(matches!(error, FrontendError::Read { .. }), "{error:?}");
    }

    #[test]
    fn errors_come_back_as_diagnostics_rather_than_as_a_failure() {
        let dir = TempDir::new("bad");
        let path = dir.write(
            "main.kira",
            "@Main function main() { print(missing) return }",
        );
        let compiled = compile(&path).expect("compile");
        assert!(compiled.has_errors());
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KSEM060"))
        );
    }

    #[test]
    fn a_types_file_outside_bind_types_is_reported() {
        let dir = TempDir::new("misplaced-bind-types");
        dir.write(
            "package.kira",
            "Package Gfx {\n    let kind = .Library\n    let moduleRoot = \"Gfx\"\n}\n",
        );
        let entry = dir.write("app/Gfx.kira", "function value() -> Int { return 1 }");
        // A `*_types.kira` file in `types/` rather than `bind-types/` is refused.
        dir.write("app/types/gfx_types.kira", "type Handle = RawPtr\n");

        let compiled = compile(&entry).expect("compile");
        assert!(
            compiled
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KPK025")),
            "{:?}",
            compiled.diagnostics
        );
    }

    #[test]
    fn a_types_file_inside_bind_types_is_accepted() {
        let dir = TempDir::new("placed-bind-types");
        dir.write(
            "package.kira",
            "Package Gfx {\n    let kind = .Library\n    let moduleRoot = \"Gfx\"\n}\n",
        );
        let entry = dir.write("app/Gfx.kira", "function value() -> Int { return 1 }");
        dir.write("app/bind-types/gfx_types.kira", "type Handle = RawPtr\n");

        let compiled = compile(&entry).expect("compile");
        assert!(
            !compiled
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KPK025")),
            "{:?}",
            compiled.diagnostics
        );
    }
}
