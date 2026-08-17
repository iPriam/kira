//! Check-only compilation of a package set held entirely in memory.
//!
//! Layer 7 of the Kira package graph.
//!
//! # What this is for
//!
//! A Kira program that wants to ask "does this package compile, and if not,
//! which diagnostic does it produce?" needs a compiler it can call and an
//! answer it can read as values. This is the compiler half of that: a
//! [`CheckSession`] takes a [`CheckRequest`] — a set of packages, each a
//! manifest text plus named source files, one of them named as the root — and
//! answers with [`CheckDiagnostic`]s carrying a code, a severity, and the file
//! they point into.
//!
//! Nothing here touches a filesystem on the caller's behalf. The only files
//! read are the bundled packages an `import` names — Foundation, which ships
//! with the toolchain — and those are read once per session and reused.
//!
//! # Why the unit is a package
//!
//! Because the interesting questions are not about one string of source. An
//! `import` is file-scoped, so `import Foundation` in one file of a package
//! must not be visible in another; a package is one flat namespace, so two of
//! its files declaring one name collide; and a library with an app on top of it
//! is two packages with an edge between them. A source-string API can state
//! none of those, and those are exactly the shapes a multi-file bug hides in.
//!
//! # Check only
//!
//! The frontend and nothing after it: name resolution and type checking, no IR
//! lowering, no bytecode, no codegen, no linker. Every diagnostic a package can
//! be asserted on is a frontend diagnostic, and a check that reached codegen
//! would make a suite of them depend on a toolchain being installed.
//!
//! # Cost
//!
//! [`CheckSession`] exists because a fail corpus is a thousand small packages,
//! not one. It caches what is shared and immutable — the bundled roots, and the
//! module sources each bundled import pulls in — so Foundation is walked and
//! read from disk once per session rather than once per call.
//!
//! It also retains the query database and re-points **one** [`SourceProgram`]
//! input at each request rather than building a fresh database per call. That
//! is what lets the frontend's per-file queries hit: macro scanning and macro
//! expansion are keyed on an interned file, so Foundation's files carry the
//! same key into every call and are expanded once per session.
//!
//! Retaining the database does not weaken isolation. Setting the input starts a
//! new salsa revision, so every whole-program query — parsing, name resolution,
//! type checking — is recomputed against this request and nothing else: call N
//! cannot see call N-1's declarations, because the flat scope those
//! declarations lived in was rebuilt from this request's files. Memory stays
//! bounded for the same reason: a whole-program memo is keyed on the one input,
//! so a new revision replaces it rather than adding to it.
//!
//! What is *not* reused yet is the whole-program half: parsing produces one
//! syntax tree with one interner spanning every file, and name resolution is
//! over one flat scope, so both are redone per call. Measured on this
//! checkout's Foundation with a release build, a two-file package that imports
//! Foundation costs about 0.95ms per call at steady state against 1.5ms with a
//! database per call, and the same package *without* the import costs 12µs.
//! Expansion is no longer part of that; parsing and name resolution are all of
//! it.

mod report;
mod request;

use std::collections::HashMap;

use kira_program_graph::bundled::{BundledRoot, bundled_roots};
use kira_runtime_abi::{CheckDiagnostic, CheckRequest};
use kira_semantics::{DiagnosticAccumulator, ModuleSource, SourceProgram, analyzed};
use salsa::Setter;

pub use request::{ResolvedRequest, resolve};

/// A reusable compiler session for checking in-memory package sets.
///
/// Holds the shared, immutable inputs — which bundled packages exist, and what
/// each bundled import pulls in — plus the query database every check runs
/// against, so a run of many checks pays for what they share once. What belongs
/// to one call lives in the input this re-points, never beside it.
pub struct CheckSession {
    /// The bundled packages an import may resolve against.
    bundles: Vec<BundledRoot>,
    /// The modules each set of bundled import names pulls in, already read.
    ///
    /// Keyed by the sorted, deduplicated import names rather than by one name:
    /// a bundled package is aggregated as a whole, so two imports of the same
    /// package must not read it twice.
    bundled: HashMap<Vec<String>, Vec<ModuleSource>>,
    /// The query database every check in this session runs against.
    database: salsa::DatabaseImpl,
    /// The one program input, re-pointed at each request.
    ///
    /// One input rather than one per call: setting it starts a new revision, so
    /// every whole-program answer is recomputed for this request and the memo
    /// holding the previous one is replaced. A second input per call would keep
    /// every previous program's tree and HIR alive forever.
    program: Option<SourceProgram>,
}

impl CheckSession {
    /// Opens a session against the bundled packages this toolchain installs.
    #[must_use]
    pub fn new() -> Self {
        Self::with_bundles(bundled_roots())
    }

    /// Opens a session against an explicit set of bundled packages.
    ///
    /// The seam a test stands its own Foundation up through, so a check does
    /// not depend on whichever toolchain the machine happens to have installed.
    #[must_use]
    pub fn with_bundles(bundles: Vec<BundledRoot>) -> Self {
        Self {
            bundles,
            bundled: HashMap::new(),
            database: salsa::DatabaseImpl::new(),
            program: None,
        }
    }

    /// Checks one package set, answering with every diagnostic it produced.
    ///
    /// Total: a package that does not compile answers with its problems, and a
    /// request that cannot be read answers with a `KPK*` diagnostic saying so.
    /// Nothing here returns an error, because a caller asking whether something
    /// compiles is entitled to an answer either way.
    pub fn check(&mut self, request: &CheckRequest) -> Vec<CheckDiagnostic> {
        let resolved = self.resolve(request);
        let ResolvedRequest {
            entry,
            modules,
            build_kind,
            mut diagnostics,
        } = resolved;
        let Some(entry) = entry else {
            return report::flatten(&diagnostics, &[], None);
        };

        let source = self.point_at(&entry, &modules, build_kind);
        let _ = analyzed(&self.database, source);
        diagnostics.extend(
            analyzed::accumulated::<DiagnosticAccumulator>(&self.database, source)
                .into_iter()
                .map(|accumulated| accumulated.0.clone()),
        );

        report::flatten(&diagnostics, &modules, Some(&entry.path))
    }

    /// Points this session's one program input at `entry` and `modules`.
    ///
    /// Setting every field is what makes the request the whole of what the
    /// frontend sees: a field left over from the previous call would be a way
    /// for one check's sources to reach another's, and salsa's revision bump is
    /// what forces every whole-program answer to be recomputed from these
    /// files.
    fn point_at(
        &mut self,
        entry: &request::EntryFile,
        modules: &[ModuleSource],
        build_kind: kira_semantics::BuildKind,
    ) -> SourceProgram {
        let Some(program) = self.program else {
            let program = SourceProgram::new(
                &self.database,
                entry.text.clone(),
                entry.path.clone(),
                modules.to_vec(),
                build_kind,
                kira_semantics::PrecompiledShaders::default(),
                kira_semantics::BuildMachine::host(),
                // Not a lint run: this path answers about code as it is written.
                false,
            );
            self.program = Some(program);
            return program;
        };
        program.set_text(&mut self.database).to(entry.text.clone());
        program.set_path(&mut self.database).to(entry.path.clone());
        program.set_modules(&mut self.database).to(modules.to_vec());
        program.set_build_kind(&mut self.database).to(build_kind);
        program
            .set_shaders(&mut self.database)
            .to(kira_semantics::PrecompiledShaders::default());
        program
            .set_machine(&mut self.database)
            .to(kira_semantics::BuildMachine::host());
        program
    }

    /// Turns a request into the entry file, modules, and request diagnostics.
    ///
    /// Public through [`CheckSession::check`] for callers, and separate here so
    /// a test can assert on which modules a request selected without running
    /// the frontend over them.
    fn resolve(&mut self, request: &CheckRequest) -> ResolvedRequest {
        request::resolve_with(request, &self.bundles, &mut self.bundled)
    }
}

impl Default for CheckSession {
    fn default() -> Self {
        Self::new()
    }
}

/// Reports what the session is configured with, not what it has memoized.
///
/// Written by hand because the query database has no `Debug`, and because
/// printing a session's memos would be printing every program it has ever been
/// asked about.
impl std::fmt::Debug for CheckSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckSession")
            .field("bundles", &self.bundles)
            .field("bundled", &self.bundled.keys())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
