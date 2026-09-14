//! Source on disk to verified IR: the one pipeline everything compiles through.
//!
//! The CLI and generated Rust crates share this pipeline. Keeping package
//! resolution, module loading, source mapping, and build-kind discovery here
//! prevents the embedding path from drifting from `kira`.
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
use salsa::Setter;

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

/// A frontend that keeps its Salsa database alive across compilations.
///
/// The one-shot [`compile_for`] API intentionally starts from a clean database
/// for callers such as `kira check`: one invocation must not inherit inputs from
/// another. A live session has the opposite requirement. It compiles the same
/// program repeatedly, and the semantics frontend already has per-file queries
/// designed to reuse unchanged expansion and parsing. Keeping the database and
/// input handle here is what makes that reuse cross a save boundary.
pub struct FrontendSession {
    db: salsa::DatabaseImpl,
    source: Option<SourceProgram>,
}

impl FrontendSession {
    /// Creates an empty incremental frontend session.
    #[must_use]
    pub fn new() -> Self {
        Self {
            db: salsa::DatabaseImpl::new(),
            source: None,
        }
    }

    /// Compiles `path`, retaining all reusable Salsa answers for the next call.
    pub fn compile_for(
        &mut self,
        path: &Path,
        kind: Option<BuildKind>,
        target: &kira_native_lib_definition::TargetTriple,
    ) -> Result<Compiled, FrontendError> {
        self.compile_for_in(path, kind, target, None)
    }

    /// Compiles `path` for `target`, naming the sysroot a cross build's
    /// bindings are generated against.
    ///
    /// The sysroot reaches the frontend for the same reason the target does:
    /// autobind runs here, and a header it parses reaches `<math.h>` and every
    /// other C library header only under the target's sysroot. The host build
    /// and an Apple target need none, so the plain `compile_for` passes `None`.
    pub fn compile_for_in(
        &mut self,
        path: &Path,
        kind: Option<BuildKind>,
        target: &kira_native_lib_definition::TargetTriple,
        sysroot: Option<&Path>,
    ) -> Result<Compiled, FrontendError> {
        compile_for_with_session(&mut self.db, &mut self.source, path, kind, target, sysroot)
    }
}

impl Default for FrontendSession {
    fn default() -> Self {
        Self::new()
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
    /// A source set refused at registration: more files than one source map
    /// can hold, or one file over the per-file size limit.
    #[error("{message}")]
    SourceLimit {
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
    compile_for_in(path, kind, target, None)
}

/// Compiles `path` for `target` against `sysroot`, the one-shot form of
/// [`FrontendSession::compile_for_in`].
pub fn compile_for_in(
    path: &Path,
    kind: Option<BuildKind>,
    target: &kira_native_lib_definition::TargetTriple,
    sysroot: Option<&Path>,
) -> Result<Compiled, FrontendError> {
    let mut session = FrontendSession::new();
    session.compile_for_in(path, kind, target, sysroot)
}

/// Compiles into an existing Salsa session, updating its one source input.
fn compile_for_with_session(
    db: &mut salsa::DatabaseImpl,
    previous: &mut Option<SourceProgram>,
    path: &Path,
    kind: Option<BuildKind>,
    target: &kira_native_lib_definition::TargetTriple,
    sysroot: Option<&Path>,
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
    let mut diagnostics = crate::autobind::run(path, target, sysroot);

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
    // The analyzer is told which machine this build is aimed at, not which one it
    // is running on. Those are the same answer for a plain `kira build` and
    // different ones for a cross build, and the difference is load-bearing twice
    // over: `Build.platform` selects platform-specific code during expansion, and
    // an `@FFI.Syscall` is refused by name on a target that cannot reach the
    // Linux kernel. Feeding the host's answers here made a
    // `--target aarch64-linux-gnu` build from Windows expand as a Windows
    // program.
    let machine = kira_semantics::BuildMachine::new(target.os(), target.arch());
    let module_paths: Vec<String> = modules.iter().map(|module| module.path.clone()).collect();
    let source = match *previous {
        Some(source) => {
            source.set_text(db).to(text);
            source.set_path(db).to(display.clone());
            source.set_modules(db).to(modules);
            source.set_build_kind(db).to(build_kind);
            source.set_shaders(db).to(shaders);
            source.set_machine(db).to(machine.clone());
            source.set_lint(db).to(lint_requested());
            source
        }
        None => {
            let source = SourceProgram::new(
                db,
                text,
                display.clone(),
                modules,
                build_kind,
                shaders,
                machine,
                lint_requested(),
            );
            *previous = Some(source);
            source
        }
    };

    // The SourceMap mirrors the salsa input file for file and in the same order,
    // so diagnostic spans render against the file they were written in: the
    // entry file at `FILE_SOURCE_ID`, then module `i` at `module_source_id(i)`.
    // It holds each file's text *after macro expansion*, because that is the
    // text the parser saw and the text every span is an offset into. A program
    // that declares no macros gets its own bytes back, so this is the file as
    // written for all but a macro-using program.
    kira_diagnostics::progress!("expanding macros");
    let expansion = kira_semantics::expanded(db, source);
    let mut sources = SourceMap::new();
    let id = sources
        .insert(display, expansion.entry.clone())
        .map_err(|error| FrontendError::SourceLimit {
            message: error.to_string(),
        })?;
    debug_assert_eq!(id, FILE_SOURCE_ID);
    for (index, path) in module_paths.into_iter().enumerate() {
        let module_text = expansion.modules.get(index).cloned().unwrap_or_default();
        let id = sources
            .insert(path, module_text)
            .map_err(|error| FrontendError::SourceLimit {
                message: error.to_string(),
            })?;
        debug_assert_eq!(id, kira_semantics::module_source_id(index));
    }
    // Then the shaders, at the ids their diagnostics were written against, so a
    // KSL error renders with the shader's own text and line.
    for (path, text) in shader_sources {
        sources
            .insert(path, text)
            .map_err(|error| FrontendError::SourceLimit {
                message: error.to_string(),
            })?;
    }

    let ir = lowered(db, source);
    kira_diagnostics::progress!("collecting diagnostics");
    diagnostics.extend(
        lowered::accumulated::<DiagnosticAccumulator>(db, source)
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
mod tests;
