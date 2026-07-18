//! Source on disk to verified IR: the one pipeline everything compiles through.
//!
//! This used to live in `kira-cli`, and moving it here is what makes the CLI a
//! driver rather than the compiler. The reason is a second consumer, not tidiness:
//! a Rust crate embedding a Kira library builds that library from its own
//! `build.rs`, and a `build.rs` that reimplemented module loading, source
//! mapping, or build-kind discovery would drift from `kirac` in exactly the ways
//! that make a bug reproduce on one path and not the other.
//!
//! # What it does not do
//!
//! It does not decide whether the program is acceptable. Diagnostics come back
//! in [`Compiled::diagnostics`] and the IR comes back regardless — lowering is
//! total — because rendering a diagnostic needs a source map and a renderer that
//! only the caller knows how to configure. [`Compiled::has_errors`] is the
//! question a caller asks; what to print, and what exit code to use, stays with
//! whoever owns the terminal.

use std::path::Path;

use kira_diagnostics::Diagnostic;
use kira_ir::IrProgram;
use kira_semantics::{BuildKind, DiagnosticAccumulator, FILE_SOURCE_ID, SourceProgram};
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
    kira_ir::lower(&program)
}

/// A compiled program plus everything needed to report on it.
#[derive(Debug)]
pub struct Compiled {
    /// Every file that took part, indexed so a span renders against its own.
    pub sources: SourceMap,
    /// Everything the frontend had to say, in source order.
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
    /// The entry file could not be read.
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
}

/// Reads and compiles `path` through the salsa frontend and IR lowering.
///
/// Returns `Err` only for problems that prevent compiling at all; compile
/// errors are carried in [`Compiled::diagnostics`], not as an error here.
pub fn compile(path: &Path) -> Result<Compiled, FrontendError> {
    let display = path.display().to_string();
    let text = std::fs::read_to_string(path).map_err(|source| FrontendError::Read {
        path: display.clone(),
        source,
    })?;

    // Everything the entry file imports, transitively, dependencies first. An
    // import that names no readable file comes back as nothing here and is
    // reported by the frontend, which has the span to point at.
    let modules = kira_program_graph::load_modules(path, &text);

    // The SourceMap mirrors the salsa input file for file and in the same order,
    // so diagnostic spans render against the file they were written in: the
    // entry file at `FILE_SOURCE_ID`, then module `i` at `module_source_id(i)`.
    let mut sources = SourceMap::new();
    let id = sources
        .insert(display.clone(), text.clone())
        .map_err(|full| FrontendError::SourceMapFull {
            message: full.to_string(),
        })?;
    debug_assert_eq!(id, FILE_SOURCE_ID);
    for (index, module) in modules.iter().enumerate() {
        let id = sources
            .insert(module.path.clone(), module.text.clone())
            .map_err(|full| FrontendError::SourceMapFull {
                message: full.to_string(),
            })?;
        debug_assert_eq!(id, kira_semantics::module_source_id(index));
    }

    let package = package_of(path)?;
    let build_kind = match package.as_ref().map(|found| found.kind()) {
        Some(kira_manifest::PackageKind::Library) => BuildKind::Library,
        Some(kira_manifest::PackageKind::App) | None => BuildKind::Application,
    };

    let db = salsa::DatabaseImpl::new();
    let source = SourceProgram::new(&db, text, display, modules, build_kind);
    let ir = lowered(&db, source);
    let diagnostics = lowered::accumulated::<DiagnosticAccumulator>(&db, source)
        .into_iter()
        .map(|accumulated| accumulated.0.clone())
        .collect();

    Ok(Compiled {
        sources,
        diagnostics,
        ir,
        build_kind,
        package_name: package.as_ref().map(|found| found.manifest.name.clone()),
        package_version: package.map(|found| found.manifest.version),
    })
}

/// The manifest governing `source`, if a `package.kira` sits above it.
///
/// The manifest is discovered by walking up from the source file, which is what
/// makes `kirac build lib/thing.kira` inside a library package build a library
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
}
