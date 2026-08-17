//! Semantic analyzer: the salsa frontend that turns source text into a typed
//! [`HirProgram`] plus diagnostics.
//!
//! Layer 2 of the Kira package graph.
//!
//! The frontend is built on salsa from the start so the language server and the
//! compiler share one query graph. The input is the entry file's text plus the
//! sibling modules it imports, already read from disk by the caller; the
//! tracked queries are [`expanded`] (macro expansion), [`parsed`] (lex +
//! parse), and [`analyzed`] (name resolution + type checking). Diagnostics are
//! never thrown — they are pushed into the [`DiagnosticAccumulator`], which
//! salsa propagates up the call graph, so a caller collects every diagnostic
//! from one `accumulated` call.
//!
//! # Two granularities
//!
//! [`SourceProgram`] is one input holding the whole program, and a caller that
//! re-points it starts a new revision: everything keyed on the program —
//! parsing, name resolution, type checking — is recomputed, which is what keeps
//! one compilation's declarations out of the next one's scope.
//!
//! Below that sits a per-**file** layer keyed on [`SourceFile`], an interned
//! handle whose identity is the file's own bytes. Two compilations that both
//! pull in Foundation hand the same bytes over and get the same handle, so the
//! answers already computed for it are found rather than recomputed.
//!
//! Expansion and parsing both live at that granularity. A file is parsed
//! against the base its position in the program gives it, so its handles and
//! symbols are already the program's and assembling a program renumbers
//! nothing: an unchanged dependency is parsed once per session, and the tree
//! shares its nodes by handle. Analysis does **not** live there yet — it
//! materializes one whole-program [`HirProgram`] whose `StructId`/`FuncId` are
//! flat indices into one table, so nothing about one package's analysis can be
//! reused when the program around it changes.
//!
//! # Why the caller reads the files
//!
//! Resolving `import support` to `support.kira` is a filesystem question, and
//! this crate has no filesystem: it sits below [`kira_vm_runtime`] in the
//! layering and must keep compiling for `wasm32-unknown-unknown`. So module
//! *loading* is injected — `kira-program-graph` walks the imports and hands the
//! texts in — while module *resolution* (which import binds which name, and
//! which import binds nothing) stays here, where the diagnostics belong.

mod aliases;
mod analyze;
mod arrays;
mod build_kind;
mod build_machine;
mod cells;
mod classes;
mod closures;
mod coercion;
mod constructs;
mod conversions;
mod copyable;
mod decl;
mod definitions;
mod enums;
mod exports;
mod ffi_types;
mod foreign;
mod foreign_aggregate;
mod foreign_callback;
mod foreign_field;
mod generics;
mod imports;
mod mutation;
mod operators;
mod ownership;
mod place;
pub(crate) mod stmt;
mod strings;
mod syscall;
mod tasks;
mod typeck;
mod types;

pub use analyze::{Analysis, analyze};
pub use build_kind::BuildKind;
pub use build_machine::{BuildMachine, host_architecture, host_platform};
pub use definitions::DefinitionLink;
pub use exports::exported_name;
pub use imports::{FileImports, ImportTable};
/// The precompiled shaders a [`SourceProgram`] carries.
///
/// Re-exported rather than restated: it is a field type of this crate's one
/// salsa input, so a caller building a program should not have to name the
/// crate that happens to define it.
pub use kira_macros::PrecompiledShaders;

use kira_core::Names;
use kira_diagnostics::Diagnostic;
use kira_semantics_model::HirProgram;
use kira_source::SourceId;
use kira_syntax_model::SyntaxTree;
use salsa::Accumulator;

/// The source id the program's **entry** file is pinned at; the CLI mirrors it
/// in its [`kira_source::SourceMap`] so diagnostic spans render against the
/// file.
///
/// Imported modules take the ids after it, in the order they were loaded — see
/// [`module_source_id`].
pub const FILE_SOURCE_ID: SourceId = SourceId::new(0);

/// The source id of the module at `index` in a [`SourceProgram`]'s module list.
///
/// A total function of the index rather than a lookup: the entry file owns id
/// 0 and module *i* owns id *i+1*, which is the same rule the caller's
/// [`kira_source::SourceMap`] follows when it inserts the entry first and the
/// modules after it in order. Both sides computing the same function is what
/// keeps a diagnostic pointing at the file it was written in.
#[must_use]
pub fn module_source_id(index: usize) -> SourceId {
    SourceId::new(index as u32 + 1)
}

/// One module the program is built from, already read from disk.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct ModuleSource {
    /// The dotted module path the import wrote (`support`, `Foundation.Web`).
    ///
    /// This is the key an `import` resolves against, not a file path: two files
    /// importing the same module name get the same module.
    pub module: String,
    /// The path the module was loaded from (for diagnostics).
    pub path: String,
    /// The module's full source text.
    pub text: String,
}

/// The one salsa input: the entry file plus the modules it imports.
#[salsa::input]
pub struct SourceProgram {
    /// The entry file's full source text.
    #[returns(clone)]
    pub text: String,
    /// The path the entry file was loaded from (for diagnostics).
    #[returns(clone)]
    pub path: String,
    /// The imported modules, dependencies before dependents.
    ///
    /// The order is load order and it is the order the modules' items appear in
    /// the tree, so a module may name a struct declared in a module it imports.
    #[returns(clone)]
    pub modules: Vec<ModuleSource>,
    /// Whether this program is being analyzed as an application or a library.
    ///
    /// A salsa *input* rather than a parameter threaded through the queries:
    /// changing it must invalidate analysis exactly as changing the text does,
    /// and an input field is what makes that automatic. The caller that read
    /// the manifest sets it; a caller with no manifest leaves it at
    /// [`BuildKind::Application`].
    pub build_kind: BuildKind,
    /// The shaders compiled before analysis ran, for the `Ksl` namespace a
    /// `comptime macro` reaches during expansion.
    ///
    /// An input rather than a parameter for the same reason `build_kind` is:
    /// changing a shader has to invalidate analysis exactly as changing the
    /// text does. Compiling one reads files, which a salsa query may not, so
    /// the build layer does it up front and hands the results in here — empty
    /// for every caller that has no shaders.
    #[returns(clone)]
    pub shaders: PrecompiledShaders,
    /// The machine this build targets: its operating system, behind
    /// `Build.platform`, and its architecture.
    ///
    /// An input for the same reason the others are: a program built for macOS
    /// and one built for Windows can expand to different code, and one built for
    /// aarch64 refuses declarations a build for x86_64 accepts, so changing it
    /// has to invalidate analysis.
    #[returns(clone)]
    pub machine: BuildMachine,
    /// Whether this compilation was asked for by `kira lint`, behind
    /// `Build.linting`.
    ///
    /// An input rather than something a macro reads from the environment: the
    /// collector query is memoized, so an environment read inside it would fix
    /// lint mode to whatever the first compilation saw. As an input, turning it
    /// on invalidates analysis exactly as changing the platform does.
    ///
    /// It is what keeps lints out of every *other* verb. A lint runs during
    /// expansion, so without this every `check`, `run` and `build` would pay
    /// for the whole lint pass and report its findings.
    pub lint: bool,
}

impl SourceProgram {
    /// Creates a single-file application: an entry file that imports nothing.
    pub fn single(db: &dyn salsa::Database, text: String, path: String) -> Self {
        Self::new(
            db,
            text,
            path,
            Vec::new(),
            BuildKind::Application,
            PrecompiledShaders::default(),
            BuildMachine::host(),
            // A program built by these helpers was not asked for by `kira lint`.
            false,
        )
    }

    /// Creates a program from an entry file and its modules, analyzed as an
    /// application.
    ///
    /// The overwhelmingly common case, and the one every caller with no
    /// manifest in hand wants; a library caller spells the kind out with
    /// [`SourceProgram::new`].
    pub fn application(
        db: &dyn salsa::Database,
        text: String,
        path: String,
        modules: Vec<ModuleSource>,
    ) -> Self {
        Self::new(
            db,
            text,
            path,
            modules,
            BuildKind::Application,
            PrecompiledShaders::default(),
            BuildMachine::host(),
            // A program built by these helpers was not asked for by `kira lint`.
            false,
        )
    }
}

/// A diagnostic emitted by any frontend query.
///
/// Wrapping [`Diagnostic`] as a salsa accumulator lets every stage report
/// without threading a sink; `query::accumulated::<DiagnosticAccumulator>`
/// gathers them, including those from called queries.
#[salsa::accumulator]
#[derive(Debug, Clone)]
pub struct DiagnosticAccumulator(pub Diagnostic);

/// A reference→definition link recorded while [`analyzed`] resolved names.
///
/// The language server's go-to-definition reads these; they ride the same
/// accumulator mechanism diagnostics do, so an editor jump is served by the
/// resolution the compiler actually performed rather than a second one.
#[salsa::accumulator]
#[derive(Debug, Clone)]
pub struct DefinitionAccumulator(pub DefinitionLink);

/// Every file of the program after macro expansion, in source-id order.
///
/// Expansion is a source-to-source transform, so what the parser sees — and
/// what a diagnostic's span is an offset into — is this text rather than what
/// was read from disk. A program that declares no macros gets its own bytes
/// back, so nothing changes for one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpandedProgram {
    /// The entry file's text, at [`FILE_SOURCE_ID`].
    pub entry: String,
    /// Each module's text, module *i* at [`module_source_id`]`(i)`.
    pub modules: Vec<String>,
}

impl ExpandedProgram {
    /// The text at `source`, when `source` names a file of this program.
    pub fn text(&self, source: SourceId) -> Option<&str> {
        if source == FILE_SOURCE_ID {
            return Some(&self.entry);
        }
        self.modules
            .iter()
            .enumerate()
            .find(|&(index, _)| module_source_id(index) == source)
            .map(|(_, text)| text.as_str())
    }
}

/// One file of the program, interned so identical bytes are one key.
///
/// This is the granularity every per-file query is keyed at, and interning is
/// what makes reuse across compilations real: two compilations that both pull
/// in Foundation hand the same bytes to `SourceFile::new` and get the same
/// handle back, so the answers already computed for it are found rather than
/// recomputed.
///
/// The source id is part of the identity because a diagnostic's span is a
/// `(SourceId, Span)` pair: the same text read at a different position in the
/// program would need its diagnostics attributed elsewhere, so it is a
/// different file.
#[salsa::interned]
pub struct SourceFile<'db> {
    /// The id every span in this file is attributed to.
    pub id: SourceId,
    /// The file's full source text, as read from disk.
    #[returns(ref)]
    pub text: String,
}

/// Every macro one file declares, found without reading any other file.
///
/// Tracked per file, so a dependency whose bytes have not changed is scanned
/// once per session rather than once per compilation.
#[salsa::tracked(returns(ref))]
fn file_macros<'db>(
    db: &'db dyn salsa::Database,
    file: SourceFile<'db>,
) -> kira_macros::FileMacros {
    kira_macros::scan(*file.id(db), file.text(db))
}

/// One file's top-level declarations, for the wrapper-template pre-scan.
///
/// Queried only when the program declares a `kind { wrapper }` macro — nothing
/// else reads another file's declarations — so an ordinary program never runs
/// it. Returned by reference: it carries every declaration's source text, which
/// is the size of the file, and no caller needs a copy.
#[salsa::tracked(returns(ref))]
fn file_declarations<'db>(
    db: &'db dyn salsa::Database,
    file: SourceFile<'db>,
) -> kira_macros::FileDeclarations {
    // No path: this is keyed on the file's id and text, and only a wrapper
    // macro reads it — none of which asks where the file came from.
    kira_macros::declarations(*file.id(db), file.text(db), "")
}

/// The program-wide inputs one file's expansion depends on.
///
/// Interned rather than passed by value because it is a *query key*: every
/// file of the program expands against the same one, and two compilations that
/// share it share every answer computed under it. It names the files that
/// declare macros rather than all of them, which is what keeps a program's own
/// files out of the key when its dependencies are what declare the macros.
#[salsa::interned]
struct MacroContext<'db> {
    /// The files that declare macros, in program order.
    #[returns(ref)]
    declaring: Vec<SourceFile<'db>>,
    /// The files scanned for wrapper templates, in program order.
    ///
    /// Empty unless some macro wears `kind { wrapper }`: a wrapper is the only
    /// macro form that reads another declaration, so with none declared no
    /// file's expansion depends on any other file's contents.
    #[returns(ref)]
    templates: Vec<SourceFile<'db>>,
    /// The operating system behind `Build.platform`.
    #[returns(ref)]
    platform: String,
    /// Whether `kira lint` asked for this compilation, behind `Build.linting`.
    lint: bool,
    /// The compiled shaders behind the `Ksl` namespace.
    #[returns(ref)]
    shaders: PrecompiledShaders,
}

/// Every macro the program declares, merged from the per-file scans.
///
/// Returned by reference, and tracked so the merge is paid for once per
/// distinct set of macro-declaring files rather than once per file that
/// expands against it.
#[salsa::tracked(returns(ref))]
fn macro_environment<'db>(
    db: &'db dyn salsa::Database,
    context: MacroContext<'db>,
) -> kira_macros::MacroEnvironment {
    let macros: Vec<&kira_macros::FileMacros> = context
        .declaring(db)
        .iter()
        .map(|&file| file_macros(db, file))
        .collect();
    let templates: Vec<&kira_macros::FileDeclarations> = context
        .templates(db)
        .iter()
        .map(|&file| file_declarations(db, file))
        .collect();
    kira_macros::environment(&macros, &templates)
}

/// One file's text after every macro in it was expanded.
///
/// The unit of reuse. Expansion fixes the environment before any file runs and
/// files do not interact after that, so one file's expanded text is a function
/// of its own bytes and the environment — which is exactly what makes it a
/// memoizable answer, and what makes analyzing a dependency's macros once per
/// session rather than once per compilation possible.
#[salsa::tracked(returns(ref))]
fn expanded_file<'db>(
    db: &'db dyn salsa::Database,
    file: SourceFile<'db>,
    context: MacroContext<'db>,
) -> kira_macros::FileExpansion {
    let environment = macro_environment(db, context);
    let shaders = context.shaders(db);
    let pipeline: Option<&dyn kira_macros::ShaderCompiler> = if shaders.is_empty() {
        None
    } else {
        Some(shaders)
    };
    kira_macros::expand_one(
        file_macros(db, file),
        file.text(db),
        environment,
        pipeline,
        context.platform(db),
    )
}

/// Expands every macro in the program, accumulating expansion diagnostics.
///
/// Runs between lexing and parsing, so the declarations a macro generates are
/// resolved and type-checked exactly like hand-written ones and every backend
/// is unaffected by construction.
///
/// The work itself is per file: this query interns each file, assembles the
/// program-wide macro context, and gathers what [`expanded_file`] answers for
/// each. A file whose bytes and context are unchanged from an earlier
/// compilation in the same session is not expanded again.
#[salsa::tracked(returns(ref))]
pub fn expanded(db: &dyn salsa::Database, source: SourceProgram) -> ExpandedProgram {
    let program = program_files(db, source);
    let files = program.files(db);
    let context = *program.context(db);

    // Scanning diagnostics first, in file order, then each file's expansion
    // diagnostics — the order a whole-program sweep reported them in.
    for &file in files {
        for diagnostic in file_macros(db, file).diagnostics() {
            DiagnosticAccumulator(diagnostic.clone()).accumulate(db);
        }
    }
    let mut texts = Vec::with_capacity(files.len());
    for &file in files {
        let expansion = expanded_file(db, file, context);
        for diagnostic in &expansion.diagnostics {
            DiagnosticAccumulator(diagnostic.clone()).accumulate(db);
        }
        texts.push(expansion.text.clone());
    }
    let mut entry = texts.pop().unwrap_or_default();
    // What a collector produced is part of the entry file from here on, so this
    // text and the one `parsed` hands the parser are the same text — see
    // [`collected`] for why the output lands here rather than in a file of its
    // own.
    append_collected(&mut entry, collected(db, source));
    ExpandedProgram {
        entry,
        modules: texts,
    }
}

/// The source every `collector` macro in the program produced, concatenated.
///
/// A whole-program query, unlike [`expanded_file`], because that is the
/// question a collector answers: it is asked about every declaration the
/// program has, so its result cannot be a function of one file's bytes and
/// cannot be memoized per file.
///
/// The output is appended to the *entry* file rather than given a file of its
/// own. A collector has no site to splice into, and the entry file is the one
/// whose namespace already holds the declarations it was asked about — giving
/// it a file would mean minting a `SourceId` mid-pipeline, after the program's
/// files are fixed.
///
/// Empty for a program that declares no collector, which is every program that
/// does not ask for one.
/// The path `id` names, or `""` when it names no file of this program.
///
/// Indexed by the same total function the ids were minted from rather than
/// searched for, so a file that moved cannot silently answer with its
/// neighbour's path.
fn path_of(db: &dyn salsa::Database, source: SourceProgram, id: SourceId) -> String {
    if id == FILE_SOURCE_ID {
        return source.path(db);
    }
    let index = (id.value() as usize).wrapping_sub(1);
    source
        .modules(db)
        .get(index)
        .map(|module| module.path.clone())
        .unwrap_or_default()
}

#[salsa::tracked(returns(ref))]
fn collected(db: &dyn salsa::Database, source: SourceProgram) -> String {
    let program = program_files(db, source);
    let context = *program.context(db);
    let files = program.files(db);
    // Each file's path travels with its text: a lint that asks *where* a
    // declaration is — to leave generated bindings alone, say — has no other
    // way to know, because a `SourceId` is a number and the map that resolves
    // it is on the far side of the macro boundary.
    let texts: Vec<(SourceId, String, String)> = files
        .iter()
        .map(|&file| {
            let id = *file.id(db);
            (
                id,
                expanded_file(db, file, context).text.clone(),
                path_of(db, source, id),
            )
        })
        .collect();
    let sources: Vec<(SourceId, &str, &str)> = texts
        .iter()
        .map(|(id, text, path)| (*id, text.as_str(), path.as_str()))
        .collect();
    let environment = macro_environment(db, context);
    let shaders = context.shaders(db);
    let pipeline: Option<&dyn kira_macros::ShaderCompiler> = if shaders.is_empty() {
        None
    } else {
        Some(shaders)
    };
    // Collectors decide whether their verb is active through the compile-time
    // Build context: TestRunner reads testing, and LintRunner reads linting.
    let (appended, reported) = kira_macros::collect_program(
        environment,
        &sources,
        pipeline,
        context.platform(db),
        *source.build_kind(db) == BuildKind::Test,
        *context.lint(db),
    );
    for diagnostic in reported {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    appended
}

/// Joins a collector's output onto the entry file's expanded text.
///
/// One place, because the text `expanded` reports and the text `parsed` gives
/// the parser have to be the same one — a diagnostic pointing into generated
/// source would otherwise land at a different offset than the node it names.
fn append_collected(entry: &mut String, appended: &str) {
    if appended.is_empty() {
        return;
    }
    entry.push('\n');
    entry.push_str(appended);
}

/// The files of one program and the macro context they all expand against.
///
/// Interned so that naming it costs nothing: expansion and parsing both need
/// the same list, and computing it twice would mean interning every file's text
/// twice per compilation.
#[salsa::interned]
struct ProgramFiles<'db> {
    /// Every file, modules first and the entry file last.
    #[returns(ref)]
    files: Vec<SourceFile<'db>>,
    /// What every one of them expands against.
    context: MacroContext<'db>,
}

/// Every file of the program as an interned handle, plus the macro context.
///
/// Modules first is the order the whole frontend runs in — a declaration may
/// name a type from a module ahead of it — and the entry file is the one that
/// depends on the modules, never the other way round.
#[salsa::tracked]
fn program_files<'db>(db: &'db dyn salsa::Database, source: SourceProgram) -> ProgramFiles<'db> {
    let modules = source.modules(db);
    let mut files: Vec<SourceFile<'db>> = modules
        .into_iter()
        .enumerate()
        .map(|(index, module)| SourceFile::new(db, module_source_id(index), module.text))
        .collect();
    files.push(SourceFile::new(db, FILE_SOURCE_ID, source.text(db)));

    // A file that declares nothing is not part of the macro context, which is
    // what keeps the context — and so every dependency's expanded text — the
    // same across compilations that differ only in their own sources.
    let scans: Vec<&kira_macros::FileMacros> =
        files.iter().map(|&file| file_macros(db, file)).collect();
    let declaring: Vec<SourceFile<'db>> = files
        .iter()
        .zip(&scans)
        .filter(|(_, scan)| scan.declares_macro())
        .map(|(&file, _)| file)
        .collect();
    // A wrapper macro is the only form that reads another file's declarations,
    // so only the files carrying one of its templates join the context. Naming
    // every file instead would make each file's expansion depend on every other
    // file's bytes, and one edit anywhere would re-expand the whole program.
    let wrappers = kira_macros::wrapper_macro_names(&scans);
    let templates: Vec<SourceFile<'db>> = if wrappers.is_empty() {
        Vec::new()
    } else {
        files
            .iter()
            .copied()
            .filter(|&file| file_declarations(db, file).carries_template_for(&wrappers))
            .collect()
    };
    let context = MacroContext::new(
        db,
        declaring,
        templates,
        source.machine(db).platform().to_owned(),
        source.lint(db),
        source.shaders(db),
    );
    ProgramFiles::new(db, files, context)
}

/// A parsed program: the syntax tree and the table its symbols resolve through.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedProgram {
    /// The parsed syntax tree, spanning every file the program is built from.
    pub tree: SyntaxTree,
    /// What every identifier symbol in the tree stands for.
    ///
    /// Assembled from the files' own interners, so two files that both write
    /// `Point` hold two symbols for it. Compare resolved text, never symbols
    /// from different files.
    pub interner: Names,
}

/// Where in the program's id space one file's handles and symbols start.
///
/// Interned because it is a *query key*: a file parsed at the same base from
/// the same bytes is the same answer, and that is what makes a dependency's
/// parse reusable across compilations. The base is part of the key rather than
/// applied afterwards because every handle a node holds is already the
/// program's — assembling a program renumbers nothing.
#[salsa::interned]
struct ParseBase<'db> {
    /// The first expression, statement, and type-reference handle.
    nodes: kira_syntax_model::NodeBase,
    /// The first symbol.
    symbols: u32,
}

/// One file lexed and parsed, numbered into the program at `base`.
///
/// The unit of reuse: a file's parse depends on its own bytes, the macro
/// context they expand against, and how much of the id space precedes it —
/// never on what any other file contains. A dependency whose bytes and position
/// are unchanged is parsed once per session rather than once per compilation.
#[salsa::tracked(returns(ref))]
fn parsed_file<'db>(
    db: &'db dyn salsa::Database,
    file: SourceFile<'db>,
    context: MacroContext<'db>,
    base: ParseBase<'db>,
) -> kira_parser::FileParse {
    let expansion = expanded_file(db, file, context);
    kira_parser::parse_file(
        *file.id(db),
        &expansion.text,
        *base.nodes(db),
        *base.symbols(db),
    )
}

/// Parses the entry file together with whatever the program's collectors made.
///
/// Separate from [`parsed_file`] because its input is not one file's bytes: a
/// collector is asked about every declaration in the program, so this depends
/// on the whole program and is memoized against it rather than against the
/// entry file. A program with no collector never reaches here and keeps the
/// per-file memoization it always had.
#[salsa::tracked(returns(ref))]
fn parsed_entry<'db>(
    db: &'db dyn salsa::Database,
    source: SourceProgram,
    file: SourceFile<'db>,
    context: MacroContext<'db>,
    base: ParseBase<'db>,
) -> kira_parser::FileParse {
    let mut text = expanded_file(db, file, context).text.clone();
    append_collected(&mut text, collected(db, source));
    kira_parser::parse_file(*file.id(db), &text, *base.nodes(db), *base.symbols(db))
}

/// Lexes and parses every file of the program, accumulating lexer/parser
/// diagnostics.
///
/// Modules are parsed **before** the entry file so their declarations come
/// first in the tree: a struct field may only name a struct declared earlier,
/// and the entry file is the one that depends on the modules, never the other
/// way round.
///
/// The work itself is per file, exactly as expansion's is: this query walks the
/// files in order, hands each the base the one before it ended at, and
/// assembles what [`parsed_file`] answers. Assembly shares each file's nodes by
/// handle, so a file whose bytes, context, and position are unchanged from an
/// earlier compilation in the same session is not parsed again.
#[salsa::tracked(returns(ref))]
pub fn parsed(db: &dyn salsa::Database, source: SourceProgram) -> ParsedProgram {
    // Asked for rather than used: expansion is what reports a macro's mistakes,
    // and a parse is a parse *of* the expansion, so the diagnostics have to
    // reach a caller collecting from this query's graph. It is a memo hit —
    // every file's expansion is already computed below.
    let _ = expanded(db, source);
    let program = program_files(db, source);
    let context = *program.context(db);

    let mut base = kira_syntax_model::NodeBase::default();
    let mut symbols = 0u32;
    let files = program.files(db);
    let mut parses = Vec::with_capacity(files.len());
    // The entry file is last, and it is the one a collector's output is
    // appended to, so it takes a different path: its text is no longer a
    // function of its own bytes alone and cannot be memoized per file.
    let entry_index = files.len().saturating_sub(1);
    for (index, &file) in files.iter().enumerate() {
        let parse = if index == entry_index && !collected(db, source).is_empty() {
            parsed_entry(db, source, file, context, ParseBase::new(db, base, symbols)).clone()
        } else {
            parsed_file(db, file, context, ParseBase::new(db, base, symbols)).clone()
        };
        base = parse.end;
        symbols = parse.symbol_end;
        parses.push(parse);
    }
    let parses: Vec<&kira_parser::FileParse> = parses.iter().collect();
    for parse in &parses {
        for diagnostic in &parse.diagnostics {
            DiagnosticAccumulator(diagnostic.clone()).accumulate(db);
        }
    }

    let result = kira_parser::assemble(parses.into_iter());
    ParsedProgram {
        tree: result.tree,
        interner: result.interner,
    }
}

/// Resolves names and type-checks the program, accumulating diagnostics.
#[salsa::tracked(returns(ref))]
pub fn analyzed(db: &dyn salsa::Database, source: SourceProgram) -> HirProgram {
    let parsed = parsed(db, source);
    let modules = source.modules(db);
    let known: Vec<(String, SourceId)> = modules
        .iter()
        .enumerate()
        .map(|(index, module)| (module.module.clone(), module_source_id(index)))
        .collect();
    let analysis = analyze(
        &parsed.tree,
        &parsed.interner,
        &known,
        *source.build_kind(db),
        &source.machine(db),
    );
    for diagnostic in analysis.diagnostics {
        DiagnosticAccumulator(diagnostic).accumulate(db);
    }
    for link in analysis.definitions {
        DefinitionAccumulator(link).accumulate(db);
    }
    analysis.program
}

#[cfg(test)]
mod tests;
