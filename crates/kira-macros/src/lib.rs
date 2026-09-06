//! Macro expansion: the frontend source-to-source pass that runs after lexing
//! and before semantic analysis.
//!
//! Layer 1 of the Kira package graph.
//!
//! Kira has two macro forms. `macro` is **declarative** — it binds expression
//! fragments and substitutes them into a fixed template, with no compile-time
//! execution. `comptime macro` is **procedural** — a real compile-time function
//! that receives syntax, runs arbitrary Kira against it, and returns the syntax
//! to splice in. Both are pure frontend transforms, so **backend parity is
//! structural**: by the time the VM, the LLVM backend, the hybrid split, or the
//! WASM pipeline sees a program, every macro in it has become ordinary Kira and
//! there is no per-backend macro work to get wrong.
//!
//! # Why this pass rewrites text
//!
//! `Syntax` in the reflection API *is* source: `Declaration.syntax` is a
//! declaration's exact text, `Syntax.rewriteProperty` is a span edit that has
//! to leave untouched source byte-for-byte intact, comments included, and
//! `quote { … }` renders to source. Expressing the pass as text edits over the
//! file and re-parsing the result is therefore not a shortcut — it is the only
//! representation in which those operations mean what they are documented to
//! mean. Everything a macro removes is *blanked* rather than deleted, so every
//! byte the user wrote that survives expansion keeps the offset it started at
//! and a diagnostic about it still points at its own line.
//!
//! # Cost when nothing uses macros
//!
//! A program that declares no macros is returned byte-identical to its input
//! after one lexing pass per file. Nothing downstream can tell this pass ran.

mod body;
mod comptime_fn;
mod decl;
mod declarative;
mod diagnostics;
mod edits;
mod eval;
mod invoke;
mod ksl;
mod probe;
mod procedural;
mod quote;
mod registry;
mod rename;
mod syntax_ops;
mod tokens;
mod value;

use std::collections::{HashMap, HashSet};

use kira_diagnostics::{Code, Diagnostic};
use kira_source::SourceId;

use crate::diagnostics::Reporter;
use crate::edits::EditBuffer;
use crate::procedural::Program;
use crate::rename::Gensym;
use crate::tokens::Lexed;

pub use crate::ksl::{CompiledShader, PrecompiledShaders, ShaderCompileError, ShaderCompiler};

/// How many times expansion re-runs over its own output before giving up.
///
/// A macro may expand into a call of another macro, so expansion is a fixpoint
/// rather than a single sweep. The bound is what turns a recursive or mutually
/// recursive macro into [`KMAC010`](diagnostics::DEPTH_LIMIT) instead of a
/// hang.
const DEPTH_LIMIT: usize = 64;

/// The result of expanding every macro in a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expansion {
    /// One text per input file, in the order the files were given.
    ///
    /// A file with nothing to expand comes back exactly as it went in.
    pub texts: Vec<String>,
    /// Files a `collector` macro produced, in declaration order of the macros
    /// that produced them.
    ///
    /// Appended to the program rather than spliced into an existing file: a
    /// collector answers about the whole program and has no site, so splicing
    /// would make its output depend on file order. Empty for a program that
    /// declares no collector, which is every program that does not ask for one.
    pub appended: Vec<String>,
    /// Everything expansion reported, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Every `.ksl` path a macro call site names, with the file that named it, in
/// the order they appear.
///
/// The build layer needs these *before* expansion, because compiling a shader
/// reads files and expansion runs inside pure queries. Matched by the shape of
/// the call rather than by the macro's name — `name!("…​.ksl")` — so an engine
/// that renames its shader macro still gets its shaders compiled.
///
/// The source id travels with the path because a path is written relative to
/// the package the call site lives in, and a program is built from more than
/// one package: a library's `ksl!("Shaders/X.ksl")` names a file beside *its*
/// manifest, not beside the manifest of whichever app depends on it.
#[must_use]
pub fn shader_paths(files: &[(SourceId, &str)]) -> Vec<(SourceId, String)> {
    let mut found: Vec<(SourceId, String)> = Vec::new();
    for &(source, text) in files {
        let file = Lexed::new(source, text);
        for call in invoke::find(&file) {
            let [argument] = call.arguments.as_slice() else {
                continue;
            };
            let written = file.slice(*argument).trim();
            if !written.starts_with('"') || !written.ends_with('"') || written.len() < 2 {
                continue;
            }
            let Ok(path) = kira_lexer::decode_string_literal(written) else {
                continue;
            };
            if path.ends_with(".ksl") && !found.iter().any(|(_, known)| *known == path) {
                found.push((source, path));
            }
        }
    }
    found
}

/// Expands every macro in `files`, returning the source the rest of the
/// frontend should parse.
///
/// Total: a malformed macro is reported and left unexpanded, so the file still
/// reaches the parser and everything else wrong with it is still reported.
///
/// No KSL pipeline is supplied, so a macro that calls `Ksl.compile` is refused
/// under [`KMAC022`](diagnostics::SHADER_COMPILE). Use [`expand_with`] to hand
/// expansion one.
#[must_use]
pub fn expand(files: &[(SourceId, &str)]) -> Expansion {
    expand_with(files, None, UNKNOWN_PLATFORM)
}

/// The platform a build that did not name one reports.
///
/// A macro asking `Build.platform` still gets an answer rather than a failure;
/// it is simply one no branch matches, which is the honest result when nothing
/// said what was being built.
pub const UNKNOWN_PLATFORM: &str = "unknown";

/// Expands every macro in `files` with `shaders` behind the `Ksl` namespace.
///
/// Separate from [`expand`] because this crate is layer 1 and the KSL pipeline
/// is above it: the caller that owns the pipeline is the one that can supply
/// it. See [`ShaderCompiler`].
#[must_use]
pub fn expand_with(
    files: &[(SourceId, &str)],
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
) -> Expansion {
    let scans: Vec<FileMacros> = files
        .iter()
        .map(|&(source, text)| scan(source, None, text))
        .collect();
    let borrowed: Vec<&FileMacros> = scans.iter().collect();
    // Only a wrapper macro makes one file's expansion depend on another file's
    // declarations, so the declaration pre-scan runs only when one exists.
    let wrappers = wrapper_macro_names(&borrowed);
    let carriers: Vec<FileDeclarations> = if wrappers.is_empty() {
        Vec::new()
    } else {
        files
            .iter()
            // No path: this entry point is handed ids and text, not locations.
            .map(|&(source, text)| declarations(source, text, ""))
            .filter(|file| file.carries_template_for(&wrappers))
            .collect()
    };
    let environment = environment(&borrowed, &carriers.iter().collect::<Vec<_>>());
    let mut collected: Vec<Diagnostic> = scans
        .iter()
        .flat_map(|scan| scan.diagnostics.iter().cloned())
        .collect();
    if environment.is_empty() {
        return Expansion {
            texts: files.iter().map(|(_, text)| (*text).to_owned()).collect(),
            appended: Vec::new(),
            diagnostics: collected,
        };
    }

    let mut texts = Vec::with_capacity(files.len());
    for (index, &(_, text)) in files.iter().enumerate() {
        let expansion = expand_one(&scans[index], text, &environment, shaders, platform);
        texts.push(expansion.text);
        collected.extend(expansion.diagnostics);
    }

    // Collectors run last, over the *expanded* texts: a declaration a derive or
    // an attribute produced is as much a declaration of the program as one
    // written by hand, and a collector that ran first would not see it.
    let expanded: Vec<FileDeclarations> = files
        .iter()
        .enumerate()
        .map(|(index, &(source, _))| declarations(source, &texts[index], ""))
        .collect();
    let (appended, reported) = procedural::collect(
        &environment.registry,
        expanded.iter().flat_map(|file| file.declarations.iter()),
        shaders,
        platform,
        false,
        // `expand_with` is the id-and-text entry point: no verb asked, so no
        // lint runs under it.
        false,
    );
    collected.extend(reported);

    Expansion {
        texts,
        appended,
        diagnostics: deduplicate(collected),
    }
}

/// Runs every `collector` macro over a whole program's expanded texts.
///
/// Returns the source a collector produced, to be appended to the program's
/// entry file, and everything the collectors reported. Empty for a program that
/// declares no collector.
///
/// Separate from [`expand_one`] because a collector is the one macro form whose
/// answer is not a function of a single file: it is asked about every
/// declaration the program has. It runs over the *expanded* texts, so a
/// declaration a derive or an attribute produced is as visible to it as one
/// written by hand.
#[must_use]
pub fn collect_program(
    environment: &MacroEnvironment,
    texts: &[(SourceId, &str, &str)],
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
    testing: bool,
    lint: bool,
) -> (String, Vec<Diagnostic>) {
    let files: Vec<FileDeclarations> = texts
        .iter()
        .map(|&(source, text, path)| declarations(source, text, path))
        .collect();
    let (appended, diagnostics) = procedural::collect(
        &environment.registry,
        files.iter().flat_map(|file| file.declarations.iter()),
        shaders,
        platform,
        testing,
        lint,
    );
    (appended.join("\n"), diagnostics)
}

/// Every macro **one** file declares, found by reading that file and nothing
/// else.
///
/// This is the unit a frontend memoizes. A dependency whose bytes have not
/// changed contributes the same scan to every compilation it takes part in, so
/// finding its macros is paid for once rather than once per compilation.
#[derive(Debug, Clone, PartialEq)]
pub struct FileMacros {
    /// The file this describes.
    source: SourceId,
    /// The package that owns the file, or `None` when no package does.
    ///
    /// `None` is the program's own files, which are one flat scope. It is what
    /// tells a name declared twice in one scope from a name a nearer package
    /// deliberately declares over a further one's.
    owner: Option<String>,
    /// The macros it declares, in declaration order.
    registry: registry::FileRegistry,
    /// Everything scanning it reported.
    diagnostics: Vec<Diagnostic>,
}

impl FileMacros {
    /// The file this scan describes.
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.source
    }

    /// Whether this file declares any macro at all.
    #[must_use]
    pub fn declares_macro(&self) -> bool {
        !self.registry.is_empty()
    }

    /// Everything scanning this file reported.
    #[must_use]
    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }
}

/// Scans one file for the macros it declares.
///
/// Total, like the rest of expansion: a malformed declaration is reported and
/// dropped, and the file is still scanned to its end.
#[must_use]
pub fn scan(source: SourceId, owner: Option<&str>, text: &str) -> FileMacros {
    let file = Lexed::new(source, text);
    let mut reporter = Reporter::new();
    let registry = registry::collect_file(&file, &mut reporter);
    FileMacros {
        source,
        owner: owner.map(str::to_owned),
        registry,
        diagnostics: reporter.into_diagnostics(),
    }
}

/// The names of every macro the program registers as a wrapper.
///
/// A wrapper macro is the only form that reads *another* declaration, so this
/// is the whole of the cross-file dependency expansion has, and it is almost
/// always empty. Answering it before scanning any declaration is what keeps a
/// program with no wrapper macro from paying for a declaration pre-scan at all.
///
/// `scans` must be every file, in file order: later files override earlier ones
/// by name, exactly as merging into one registry does, so a name redeclared as
/// something other than a wrapper is not one here either.
#[must_use]
pub fn wrapper_macro_names(scans: &[&FileMacros]) -> Vec<String> {
    let mut forms: HashMap<&str, registry::ProceduralKind> = HashMap::new();
    for scan in scans {
        for declared in &scan.registry.procedural {
            forms.insert(&declared.name, declared.kind);
        }
    }
    let mut names: Vec<String> = forms
        .into_iter()
        .filter(|&(_, kind)| kind == registry::ProceduralKind::Wrapper)
        .map(|(name, _)| name.to_owned())
        .collect();
    // Sorted so the answer is a stable value: a caller keying a query on it
    // must get the same key from the same program however the map iterated.
    names.sort_unstable();
    names
}

/// The top-level declarations of one file, for the wrapper-template pre-scan.
///
/// Separate from [`FileMacros`] because it is needed only when some macro wears
/// `kind { wrapper }`. Scanning declarations costs the whole file, so a program
/// with no wrapper macro — every program so far — never runs it.
#[derive(Debug, Clone, PartialEq)]
pub struct FileDeclarations {
    /// The file this describes.
    source: SourceId,
    /// Its top-level declarations, annotations included.
    declarations: Vec<decl::Declaration>,
}

impl FileDeclarations {
    /// The file this scan describes.
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.source
    }

    /// Whether this file declares something one of `wrappers` registers as a
    /// template.
    #[must_use]
    pub fn carries_template_for(&self, wrappers: &[String]) -> bool {
        self.declarations.iter().any(|declaration| {
            declaration
                .annotations
                .iter()
                .any(|annotation| wrappers.contains(&annotation.name))
        })
    }
}

/// Scans one file's top-level declarations.
///
/// `path` is where the file was read from, and `""` says the caller does not
/// know — a lint reading it must treat the empty path as "unplaceable" rather
/// than as a path that matches nothing.
#[must_use]
pub fn declarations(source: SourceId, text: &str, path: &str) -> FileDeclarations {
    FileDeclarations {
        source,
        declarations: procedural::top_level(&Lexed::at(source, text, path)),
    }
}

/// Every macro the program declares, plus the wrapper templates they registered.
///
/// Built by merging per-file scans in file order, which is exactly what a
/// single whole-program walk produced: a later file's macro of the same name
/// wins, and a wrapper template is looked for in every declaration the caller
/// hands over.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MacroEnvironment {
    /// Every macro the program declares, by name.
    registry: registry::Registry,
    /// Every struct a `kind { wrapper }` macro registered as a template.
    templates: HashMap<String, procedural::WrapperTemplate>,
    /// Names declared twice inside one scope, reported once for the program.
    conflicts: Vec<Diagnostic>,
}

impl MacroEnvironment {
    /// Every name declared twice inside one scope.
    ///
    /// A program-wide answer rather than a per-file one, because the second
    /// declaration is only a conflict in the company of the first. Reported
    /// once by the caller that assembles the program.
    #[must_use]
    pub fn conflicts(&self) -> &[Diagnostic] {
        &self.conflicts
    }

    /// Whether the program declares no macros at all.
    ///
    /// Expansion is skipped entirely when this holds, which is what keeps a
    /// program that never mentions a macro byte-identical to its own source.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.registry.is_empty()
    }
}

/// Merges per-file scans into the program-wide macro environment.
///
/// `macros` needs to carry every file that declares a macro, and `templates`
/// every file carrying a declaration a wrapper macro would register — which is
/// none unless [`wrapper_macro_names`] found one. A file in neither contributes
/// nothing, so leaving it out changes no answer.
#[must_use]
pub fn environment(macros: &[&FileMacros], templates: &[&FileDeclarations]) -> MacroEnvironment {
    let mut registry = registry::Registry::default();
    let mut conflicts = Vec::new();
    for file in macros {
        registry.absorb(file.owner.as_deref(), &file.registry, &mut conflicts);
    }
    let templates = procedural::wrapper_templates(
        templates.iter().map(|file| file.declarations.as_slice()),
        &registry,
    );
    MacroEnvironment {
        registry,
        templates,
        conflicts,
    }
}

/// One file's text after expansion, plus what expanding it reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileExpansion {
    /// The file's text after every macro in it was expanded.
    pub text: String,
    /// Everything expanding it reported, in the order it was reported.
    pub diagnostics: Vec<Diagnostic>,
}

/// Expands every macro in one file against the program-wide `environment`.
///
/// `scan` must be [`scan`]'s answer for this file's `text` — it carries the
/// byte ranges of the file's own macro declarations, which are blanked before
/// anything in the file is expanded so a call site inside a macro's template is
/// never mistaken for a use of it.
///
/// Expansion is a fixpoint per file, not per program: a macro may expand into a
/// call of another macro, so this re-runs over its own output until the file
/// stops changing. Files do not interact during expansion — the environment is
/// fixed before any of them runs — so running each to its own fixpoint gives
/// the same texts a whole-program sweep did, and is what makes one file's
/// expansion a memoizable answer.
#[must_use]
pub fn expand_one(
    scan: &FileMacros,
    text: &str,
    environment: &MacroEnvironment,
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
) -> FileExpansion {
    if environment.is_empty() {
        return FileExpansion {
            text: text.to_owned(),
            diagnostics: Vec::new(),
        };
    }
    let source = scan.source;
    let mut current = text.to_owned();
    let mut gensym = Gensym::new();
    let mut collected: Vec<Diagnostic> = Vec::new();
    for round in 0..DEPTH_LIMIT {
        let mut reporter = Reporter::new();
        let blanks: &[kira_source::Span] = if round == 0 {
            &scan.registry.spans
        } else {
            &[]
        };
        let text = std::mem::take(&mut current);
        let file = Lexed::new(source, &text);
        let mut buffer = EditBuffer::new();
        for span in blanks {
            buffer.blank(*span, &text);
        }
        let program = Program {
            registry: &environment.registry,
            templates: &environment.templates,
            shaders,
            platform,
        };
        expand_file(
            &file,
            program,
            blanks,
            &mut gensym,
            &mut buffer,
            &mut reporter,
        );
        if buffer.is_empty() {
            collected.extend(reporter.into_diagnostics());
            current = text;
            break;
        }
        let applied = buffer.apply(&text);
        if applied.overlapped {
            reporter.error(
                source,
                kira_source::Span::new(0, 0),
                diagnostics::CONFLICTING_REWRITE,
                "two macro expansions rewrote the same source range",
            );
        }
        let changed = applied.text != text;
        collected.extend(reporter.into_diagnostics());
        current = applied.text;
        if !changed {
            break;
        }
        if round + 1 == DEPTH_LIMIT {
            collected.push(diagnostics::error(
                source,
                kira_source::Span::new(0, 0),
                diagnostics::DEPTH_LIMIT,
                format!("macro expansion did not settle after {DEPTH_LIMIT} rounds"),
            ));
        }
    }
    FileExpansion {
        text: current,
        diagnostics: collected,
    }
}

/// Records every edit one file needs this round.
///
/// `blanked` names the byte ranges the macro declarations themselves occupy: a
/// call site inside a macro's own template is part of the declaration, not a
/// use of it, and expanding it would both rewrite bytes that are about to be
/// blanked and expand a template that has no arguments bound yet.
fn expand_file(
    file: &Lexed<'_>,
    program: Program<'_>,
    blanked: &[kira_source::Span],
    gensym: &mut Gensym,
    buffer: &mut EditBuffer,
    reporter: &mut Reporter,
) {
    let comptime = program.comptime();
    let Program { registry, .. } = program;
    for declaration in procedural::top_level(file) {
        if blanked.iter().any(|span| {
            span.start <= declaration.span.start && declaration.span.end() <= span.end()
        }) {
            continue;
        }
        procedural::expand_declaration(file, &declaration, program, buffer, reporter);
    }
    // A `comptime function` call wears no `!`, so it is found by name. A call
    // sitting **inside a macro's arguments** is left for a later round: its
    // span is contained in the macro call's span, so rewriting both here would
    // overlap and be refused as a bug in this crate. The macro expands first,
    // and the literal its expansion carries is found and folded by name on the
    // next pass.
    let comptime_names = registry.comptime_function_names();
    if !comptime_names.is_empty() {
        let nested: Vec<kira_source::Span> = invoke::find(file)
            .iter()
            .filter(|call| {
                registry.declarative(&call.name).is_some()
                    || registry.procedural(&call.name).is_some()
            })
            .map(|call| call.span)
            .collect();
        let calls = invoke::find_named(file, &comptime_names);
        for call in invoke::innermost(&calls) {
            if blanked
                .iter()
                .any(|span| span.start <= call.span.start && call.span.end() <= span.end())
                || nested
                    .iter()
                    .any(|span| span.start <= call.span.start && call.span.end() <= span.end())
            {
                continue;
            }
            let Some(declared) = registry.comptime_function(&call.name) else {
                continue;
            };
            if let Some(literal) =
                comptime_fn::expand_call(file, declared, &call, comptime, reporter)
            {
                buffer.replace(call.span, literal);
            }
        }
    }
    let all = invoke::find(file);
    for call in invoke::innermost(&all) {
        if blanked
            .iter()
            .any(|span| span.start <= call.span.start && call.span.end() <= span.end())
        {
            continue;
        }
        if let Some(declared) = registry.declarative(&call.name) {
            if let Some(expanded) = declarative::expand(declared, &call, file, gensym, reporter) {
                if let Some(hoist) = expanded.hoist {
                    buffer.insert(call.statement_start, hoist);
                }
                buffer.replace(call.span, expanded.replacement);
            }
            continue;
        }
        if let Some(declared) = registry.procedural(&call.name) {
            if let Some(expansion) =
                procedural::expand_call(file, declared, &call, comptime, reporter)
            {
                match call.position {
                    invoke::Position::Declaration => {
                        buffer.blank(call.span, file.text);
                        buffer.append(&expansion);
                    }
                    invoke::Position::Statement | invoke::Position::Expression => {
                        buffer.replace(call.span, expansion);
                    }
                }
            }
            continue;
        }
        reporter.error(
            file.source,
            call.name_span,
            diagnostics::UNKNOWN_MACRO,
            format!("`{}` is not a macro", call.name),
        );
    }
}

/// Drops repeats of the same reported problem.
///
/// Expansion is a fixpoint, so a call site that could not be expanded is seen
/// again on every following round and would otherwise report once per round.
/// Two genuinely distinct sites with the same code and the same message are the
/// same problem stated twice, so collapsing them loses nothing.
fn deduplicate(items: Vec<Diagnostic>) -> Vec<Diagnostic> {
    let mut seen: HashSet<(Option<Code>, String)> = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert((item.code.clone(), item.message.clone())))
        .collect()
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
