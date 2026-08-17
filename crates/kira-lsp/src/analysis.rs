//! Analysis: one document's text in, this compiler's diagnostics out.
//!
//! The point of this module is that it contains no analysis. The language
//! server runs the *same* salsa frontend `kira check` runs, so an editor
//! squiggle and a command-line error are the same computation and cannot drift
//! into two opinions about one program.
//!
//! # Why `analyzed` and not `lowered`
//!
//! The CLI collects diagnostics under its `lowered` query, which calls
//! `analyzed` and then lowers to IR. Lowering contributes no diagnostics of its
//! own — `kira-ir` does not even depend on salsa — so everything a user would
//! see from `kira check` is already accumulated under `analyzed`. Reaching for
//! IR here would make the server depend on a backend crate to learn nothing.

use kira_diagnostics::Diagnostic;
use kira_semantics::{
    BuildKind, DefinitionAccumulator, DefinitionLink, DiagnosticAccumulator, FILE_SOURCE_ID,
    ModuleSource, SourceProgram, module_source_id,
};
use kira_source::{SourceFile, SourceId};
use salsa::Setter;

/// The query database every analysis in this server runs against.
///
/// One database and one input for the life of the server, rather than a fresh
/// pair per keystroke. Setting the input starts a new salsa revision, so the
/// document being edited is re-expanded, re-parsed, and re-analyzed — nothing
/// stale can survive an edit — while the per-file answers for every module the
/// edit did not touch are found rather than recomputed.
pub struct AnalysisSession {
    /// The database the frontend's memos live in.
    database: salsa::DatabaseImpl,
    /// The one program input, re-pointed at each document analyzed.
    program: Option<SourceProgram>,
}

impl AnalysisSession {
    /// Opens a session with nothing analyzed yet.
    pub fn new() -> Self {
        Self {
            database: salsa::DatabaseImpl::new(),
            program: None,
        }
    }

    /// Points the one program input at this document and its modules.
    ///
    /// Every field is set, so what the frontend sees is this document and
    /// nothing left over from the last one.
    fn point_at(
        &mut self,
        path: &str,
        text: &str,
        modules: Vec<ModuleSource>,
        build_kind: BuildKind,
    ) -> SourceProgram {
        let Some(program) = self.program else {
            let program = SourceProgram::new(
                &self.database,
                text.to_owned(),
                path.to_owned(),
                modules,
                build_kind,
                kira_semantics::PrecompiledShaders::default(),
                kira_semantics::BuildMachine::host(),
                // The language server never lints: it answers about code as written.
                false,
            );
            self.program = Some(program);
            return program;
        };
        program.set_text(&mut self.database).to(text.to_owned());
        program.set_path(&mut self.database).to(path.to_owned());
        program.set_modules(&mut self.database).to(modules);
        program.set_build_kind(&mut self.database).to(build_kind);
        program
            .set_shaders(&mut self.database)
            .to(kira_semantics::PrecompiledShaders::default());
        program
            .set_machine(&mut self.database)
            .to(kira_semantics::BuildMachine::host());
        program
    }
}

impl Default for AnalysisSession {
    fn default() -> Self {
        Self::new()
    }
}

/// One analyzed document: its diagnostics, and the file they point into.
pub struct Analysis {
    /// Every diagnostic the frontend accumulated, in source order.
    pub diagnostics: Vec<Diagnostic>,
    /// The analyzed text, with the line table for mapping spans to positions.
    pub file: SourceFile,
    /// Every reference the analyzer resolved, linked to its definition.
    pub definitions: Vec<DefinitionLink>,
    /// Every file of the program — the document first, its modules after —
    /// indexed so `files[source.value()]` is the file a [`SourceId`] names.
    ///
    /// That indexing is [`module_source_id`]'s rule, mirrored here the same
    /// way the CLI's `SourceMap` mirrors it: the entry file owns id 0 and
    /// module *i* owns id *i + 1*. It is what turns a definition's `FileSpan`
    /// in an imported module into that module's path and line table.
    pub files: Vec<SourceFile>,
}

/// The source id the document being edited is analyzed under.
///
/// A program is no longer one file: the document is the *entry* file, and every
/// module it imports is analyzed alongside it under a later id. This server
/// publishes diagnostics for one document at a time — that is what the protocol
/// asks of it — so a diagnostic pointing into an imported module belongs to a
/// different file's squiggles and is dropped here rather than misplaced onto
/// this one. Opening that module analyzes it in turn and shows the same
/// diagnostic where it lives.
pub const DOCUMENT_SOURCE: SourceId = FILE_SOURCE_ID;

/// Analyzes one document's text as the whole program its package makes it.
///
/// The document's own directory is the module root, so `import support` in
/// `~/app/main.kira` reads `~/app/support.kira` **from disk** — the version on
/// disk, not an unsaved editor buffer. That is a real limitation and a
/// deliberate one: routing an open buffer into the module set means the server
/// owning a document store keyed by module path, and a wrong answer from a
/// stale buffer is worse than a right answer from a saved file.
///
/// A document that belongs to a package is analyzed *as* that package. The
/// server runs the same assembly step `kira build` does, so a `package.kira`
/// above the file — one directory up from `app/main.kira`, or any number of
/// them up in a nested tree — contributes its resolved dependencies, its
/// sibling sources, and its declared kind. Without that step an editor reported
/// every dependency import as an unresolved library and every sibling
/// declaration as undefined, on a tree `kira check` compiled clean.
///
/// Assembly that fails outright — an unreadable or malformed `package.kira`, a
/// dependency path that resolves to nothing — falls back to analyzing the
/// document as a lone application. The alternative is an editor that goes blank
/// while a manifest is half-typed, and a lone-file analysis is the answer the
/// server gave for every document before it learned about packages.
///
/// `import Foundation` needs nothing here. The loader resolves a bundled
/// package the same way for the server as for the compiler — relative to the
/// running executable, so an installed `kira-lsp` sitting in a toolchain's
/// `bin/` reads that toolchain's Foundation — and the editor gets completion
/// and diagnostics over the standard library without this crate knowing it
/// exists. A server that could not find a bundle simply reports the import
/// unresolved, which is what an editor should say about a broken install.
///
/// Analyzed through a session, so the frontend's per-file answers survive a
/// keystroke. Setting the input starts a new revision and every whole-program
/// answer is recomputed, but a module the edit did not touch — every file of
/// Foundation, every file of every dependency — keeps the same interned key and
/// is not expanded again. That is the difference between an editor that pays
/// for the whole program on each character and one that pays for the file being
/// typed in.
pub fn analyze(session: &mut AnalysisSession, path: &str, text: &str) -> Analysis {
    let entry = std::path::Path::new(path);
    let (modules, build_kind) = match kira_program_graph::load_program(entry, text) {
        Ok(assembled) => (assembled.modules, assembled.build_kind),
        Err(_) => (
            kira_program_graph::load_modules(entry, text),
            BuildKind::Application,
        ),
    };
    // The per-file mirror is built before the module list moves into salsa:
    // it is what maps a definition's `SourceId` back to a path and a line
    // table when a jump lands in an imported module.
    let mut files = vec![SourceFile::new(
        FILE_SOURCE_ID,
        path.to_owned(),
        text.to_owned(),
    )];
    files.extend(modules.iter().enumerate().map(|(index, module)| {
        SourceFile::new(
            module_source_id(index),
            module.path.clone(),
            module.text.clone(),
        )
    }));
    // A document with no manifest above it is an application, which is what
    // keeps the missing-`@Main` error where it belongs for a bare file. A
    // library package says so in its own `package.kira`, and its author no
    // longer sees a spurious `KSEM011` for a file that was never meant to have
    // an entrypoint.
    let source = session.point_at(path, text, modules, build_kind);
    let db = &session.database;
    let _ = kira_semantics::analyzed(db, source);
    let diagnostics = kira_semantics::analyzed::accumulated::<DiagnosticAccumulator>(db, source)
        .into_iter()
        .map(|accumulated| accumulated.0.clone())
        // A diagnostic in an imported module is that module's to show. The
        // conversion layer drops any label outside `DOCUMENT_SOURCE` anyway;
        // filtering here is what keeps a module's error from arriving as a
        // span-less entry pinned to line 1 of this document.
        .filter(|diagnostic: &Diagnostic| {
            diagnostic
                .labels
                .iter()
                .any(|label| label.span.source == DOCUMENT_SOURCE)
        })
        .collect();
    let definitions = kira_semantics::analyzed::accumulated::<DefinitionAccumulator>(db, source)
        .into_iter()
        .map(|accumulated| accumulated.0)
        .collect();

    Analysis {
        diagnostics,
        file: SourceFile::new(DOCUMENT_SOURCE, path.to_owned(), text.to_owned()),
        definitions,
        files,
    }
}

#[cfg(test)]
mod tests;
