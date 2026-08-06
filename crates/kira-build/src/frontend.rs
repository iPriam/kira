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

use std::path::{Path, PathBuf};

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
    kira_diagnostics::progress!("analyzing");
    let program = kira_semantics::analyzed(db, source);
    kira_diagnostics::progress!("lowering to IR");
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
    /// The program's source set could not be assembled from the tree.
    ///
    /// Manifest discovery, dependency resolution, and reading a package's own
    /// sources all report through here, because all three happen inside the one
    /// assembly step this frontend shares with the language server.
    #[error(transparent)]
    Assembly(#[from] kira_program_graph::AssemblyError),
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
    compile_for(path, kind, &kira_project::host_target())
}

/// Compiles `path` for `target`, which decides what its C bindings are
/// generated against.
///
/// The target reaches the frontend because autobind runs inside it: a `long` is
/// 32 bits under MSVC and 64 elsewhere, so the same header produces a different
/// binding per target, and the binding is Kira source the analyzer reads. Every
/// other decision the target drives happens after this and takes it again.
pub fn compile_for(
    path: &Path,
    kind: Option<BuildKind>,
    target: &kira_native_lib_definition::TargetTriple,
) -> Result<Compiled, FrontendError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| FrontendError::Read {
        path: display.clone(),
        source,
    })?;
    kira_diagnostics::progress!("resolving packages");
    // Manifest-declared bindings are generated first, because they are Kira
    // source: a `@FFI.Extern` that does not exist on disk when the module walk
    // runs is an undefined function at every call site, blaming the caller for
    // a file the build was supposed to write. Failures come back as
    // diagnostics — a program that cannot bind one library still has every
    // other diagnostic worth reporting.
    let mut diagnostics = crate::autobind::run(path, target);

    // Discovery, dependency resolution, module loading, and package-member
    // aggregation are one step shared with the language server: an editor and
    // `kira check` must assemble the same program from the same tree.
    kira_diagnostics::progress!("loading modules");
    let assembled = kira_program_graph::load_program(path, &text)?;
    let package = assembled.package;
    let modules = assembled.modules;
    diagnostics.extend(assembled.diagnostics);

    // A lockfile that drifted is rewritten here rather than inside assembly:
    // resolution never writes, and a language server assembling the same
    // program on a keystroke must not touch the tree.
    if let Some(graph) = assembled.graph.as_ref()
        && let Some(found) = package.as_ref()
    {
        sync_drifted_lockfile(
            &package_root_dir(found),
            &graph.lockfile,
            &graph.packages,
            &mut diagnostics,
        );
    }

    // Enforce the `bind-types/` convention across every loaded source: a
    // `*_types.kira` foreign-binding vocabulary file must sit in a `bind-types/`
    // directory. Reported here, the one place the entry and every loaded module
    // path converge with the diagnostics channel.
    diagnostics.extend(bind_types_placement_diagnostics(path, &modules));

    let build_kind = kind.unwrap_or(assembled.build_kind);

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
    // Which package each module was loaded from, so a `ksl!` written in a
    // DEPENDENCY resolves against that dependency's manifest. Modules arrive as
    // file paths, so the package is the nearest ancestor directory holding a
    // manifest; a module with none falls back to the root package below.
    let shader_roots: Vec<(kira_source::SourceId, PathBuf)> = modules
        .iter()
        .enumerate()
        .filter_map(|(index, module)| {
            package_root_of(Path::new(&module.path))
                .map(|directory| (kira_semantics::module_source_id(index), directory))
        })
        .collect();
    // Shader sources are numbered after the entry file and every module, which
    // is where the `SourceMap` below has room for them.
    let shader_base = u32::try_from(modules.len() + 1).unwrap_or(u32::MAX);
    let (shaders, shader_diagnostics, shader_sources) =
        crate::shader::precompile(&shader_root, &shader_roots, &shader_files, shader_base);
    diagnostics.extend(shader_diagnostics);
    drop(shader_files);

    kira_diagnostics::progress!("indexing sources");
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
        lint_requested(),
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

    let ir = lowered(&db, source);
    kira_diagnostics::progress!("collecting diagnostics");
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

/// The package a source file belongs to: the nearest ancestor directory holding
/// a manifest.
///
/// A module is loaded by file path, and the package it came from is not carried
/// alongside it — but anything written relative to "the package" (a `ksl!`
/// shader path) has to resolve against that package rather than against whoever
/// is building. Walking up from the file is what finds it, and a file with no
/// manifest above it belongs to no package, which the caller reads as "use the
/// root package's directory".
fn package_root_of(file: &Path) -> Option<PathBuf> {
    let mut directory = file.parent()?;
    loop {
        if directory.join("package.kira").is_file() {
            return Some(directory.to_path_buf());
        }
        directory = directory.parent()?;
    }
}

/// The directory a manifest governs, which is the directory it sits in.
fn package_root_dir(package: &kira_project::Manifest) -> PathBuf {
    match Path::new(&package.path).parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
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
                !diagnostic.has_code(DiagnosticCode::Kpk024LockfileDrift.as_str())
            });
            diagnostics.push(lockfile_synced(&display));
        }
        Err(error) => diagnostics.push(lockfile_sync_failed(&display, &error.to_string())),
    }
}

/// Whether `kira lint` asked for this compilation.
///
/// Read from the environment here, before analysis begins, rather than inside a
/// macro: the collector query is memoized, and an environment read inside it
/// would fix lint mode to whatever the first compilation in the process saw.
/// Read once, at the edge, it becomes an ordinary salsa input.
fn lint_requested() -> bool {
    std::env::var_os(LINT_MODE).is_some()
}

/// The variable `kira lint` sets on itself before compiling.
pub const LINT_MODE: &str = "KIRA_LINT";

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself, so a failing test leaves no
    /// litter and no test depends on another's leftovers.
    struct TempDir(PathBuf);

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

        fn write(&self, name: &str, text: &str) -> PathBuf {
            // Pushed component by component rather than joined whole: `join`
            // keeps an embedded `/` verbatim on Windows, so `app/Core.kira`
            // would produce a path spelled with a separator the rest of the
            // toolchain never emits, and comparing it against one `kira-project`
            // built would fail on Windows only.
            let mut path = self.0.clone();
            for component in name.split('/') {
                path.push(component);
            }
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
                .any(|diagnostic| diagnostic.has_code("KPK026")),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.has_code("KPK024")),
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
                .any(|diagnostic| diagnostic.has_code("KSEM032")),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled.diagnostics.iter().any(|diagnostic| {
                diagnostic.has_code("KSEM060") && diagnostic.message.contains("coreValue")
            }),
            "{:?}",
            compiled.diagnostics
        );
        assert!(
            !compiled.diagnostics.iter().any(|diagnostic| {
                diagnostic
                    .code
                    .as_ref()
                    .is_some_and(|code| code.as_str().starts_with("KPK"))
            }),
            "{:?}",
            compiled.diagnostics
        );

        let library_diagnostic = compiled
            .diagnostics
            .iter()
            .find(|diagnostic| {
                diagnostic.has_code("KSEM060") && diagnostic.message.contains("missingFromCore")
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
            .find(|diagnostic| diagnostic.has_code("KPK020"))
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
        assert!(compiled.diagnostics.iter().any(|d| d.has_code("KSEM060")));
    }

    /// The whole point of running autobind inside the frontend: a package that
    /// declares `autobind` and ships no bindings compiles anyway, because the
    /// bindings are written before a module is loaded. Without it, every call
    /// into the C library is an undefined function and the caller gets blamed
    /// for a file the build was supposed to write.
    #[test]
    fn a_declared_binding_is_generated_before_the_call_to_it_is_analyzed() {
        let dir = TempDir::new("autobind");
        dir.write(
            "package.kira",
            "Package demo {\n\
             \x20   let version = \"0.1.0\"\n\
             \x20   let kind = PackageKind.App\n\
             \x20   let nativeLibraries = [\n\
             \x20       NativeLibrary {\n\
             \x20           name: \"demo\",\n\
             \x20           linkMode: LinkMode.Static,\n\
             \x20           autobind: Autobind { module: \"demo\", headers: [\"NativeLibs/demo.h\"], mode: AutobindMode.AllPublic },\n\
             \x20           nativeTargets: [\n\
             \x20               NativeTarget { triple: \"HOST_TRIPLE\", staticLib: \"generated/libdemo.a\" }\n\
             \x20           ],\n\
             \x20       }\n\
             \x20   ]\n\
             }\n"
                .replace("HOST_TRIPLE", &kira_project::host_target().to_string())
                .as_str(),
        );
        dir.write(
            "NativeLibs/demo.h",
            "double demo_measure(const char *text, double size);\n",
        );
        let entry = dir.write(
            "app/main.kira",
            "@Main function main() { print(demo_measure(\"hi\", 14.0)) return }",
        );

        let compiled = compile(&entry).expect("compile");
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        let generated = std::fs::read_to_string(dir.0.join("app/bindings/demo.kira"))
            .expect("the binding was written into the package");
        assert!(generated.contains("symbol: demo_measure"), "{generated}");
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
            compiled.diagnostics.iter().any(|d| d.has_code("KPK025")),
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
            !compiled.diagnostics.iter().any(|d| d.has_code("KPK025")),
            "{:?}",
            compiled.diagnostics
        );
    }
}
