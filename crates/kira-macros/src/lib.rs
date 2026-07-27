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

use std::collections::HashSet;

use kira_diagnostics::Diagnostic;
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
    /// Everything expansion reported, in source order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Every `.ksl` path a macro call site names, in the order they appear.
///
/// The build layer needs these *before* expansion, because compiling a shader
/// reads files and expansion runs inside pure queries. Matched by the shape of
/// the call rather than by the macro's name — `name!("…​.ksl")` — so an engine
/// that renames its shader macro still gets its shaders compiled.
#[must_use]
pub fn shader_paths(files: &[(SourceId, &str)]) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
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
            let path = kira_lexer::decode_string_literal(written);
            if path.ends_with(".ksl") && !found.contains(&path) {
                found.push(path);
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
    let identity: Vec<String> = files.iter().map(|(_, text)| (*text).to_owned()).collect();
    let lexed: Vec<Lexed<'_>> = files
        .iter()
        .map(|&(source, text)| Lexed::new(source, text))
        .collect();

    let mut reporter = Reporter::new();
    let (registry, declarations) = registry::collect(&lexed, &mut reporter);
    if registry.is_empty() {
        return Expansion {
            texts: identity,
            diagnostics: reporter.into_diagnostics(),
        };
    }

    let templates = procedural::wrapper_templates(&lexed, &registry);
    let mut texts = identity;
    let mut gensym = Gensym::new();
    let mut collected: Vec<Diagnostic> = reporter.into_diagnostics();
    let mut exhausted = true;
    for round in 0..DEPTH_LIMIT {
        let mut reporter = Reporter::new();
        let mut changed = false;
        for (index, &(source, _)) in files.iter().enumerate() {
            let blanks: Vec<kira_source::Span> = if round == 0 {
                declarations
                    .iter()
                    .filter(|declaration| declaration.source == source)
                    .map(|declaration| declaration.span)
                    .collect()
            } else {
                Vec::new()
            };
            let text = texts[index].clone();
            let file = Lexed::new(source, &text);
            let mut buffer = EditBuffer::new();
            for span in &blanks {
                buffer.blank(*span, &text);
            }
            let program = Program {
                registry: &registry,
                templates: &templates,
                shaders,
                platform,
            };
            expand_file(
                &file,
                program,
                &blanks,
                &mut gensym,
                &mut buffer,
                &mut reporter,
            );
            if buffer.is_empty() {
                continue;
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
            if applied.text != text {
                changed = true;
            }
            texts[index] = applied.text;
        }
        collected.extend(reporter.into_diagnostics());
        if !changed {
            exhausted = false;
            break;
        }
        if round + 1 == DEPTH_LIMIT {
            let (source, _) = files.first().copied().unwrap_or((SourceId::new(0), ""));
            collected.push(diagnostics::error(
                source,
                kira_source::Span::new(0, 0),
                diagnostics::DEPTH_LIMIT,
                format!("macro expansion did not settle after {DEPTH_LIMIT} rounds"),
            ));
        }
    }
    let _ = exhausted;

    Expansion {
        texts,
        diagnostics: deduplicate(collected),
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
    let Program {
        registry,
        shaders,
        platform,
        ..
    } = program;
    for declaration in procedural::top_level(file) {
        if blanked.iter().any(|span| {
            span.start <= declaration.span.start && declaration.span.end() <= span.end()
        }) {
            continue;
        }
        procedural::expand_declaration(file, &declaration, program, buffer, reporter);
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
                procedural::expand_call(file, declared, &call, shaders, platform, reporter)
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
    let mut seen: HashSet<(Option<&'static str>, String)> = HashSet::new();
    items
        .into_iter()
        .filter(|item| seen.insert((item.code, item.message.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expand_one(text: &str) -> Expansion {
        expand(&[(SourceId::new(0), text)])
    }

    #[test]
    fn a_program_with_no_macros_is_returned_unchanged() {
        let program = "@Main function main() {\n    print(1)\n    return\n}\n";
        let expansion = expand_one(program);
        assert_eq!(expansion.texts[0], program);
        assert!(expansion.diagnostics.is_empty());
    }

    #[test]
    fn a_macro_declaration_is_blanked_and_keeps_every_other_offset() {
        let program = "macro square(value: expr) { expand { value * value } }\n\
                       @Main function main() {\n    print(square!(6))\n    return\n}\n";
        let expansion = expand_one(program);
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        let expanded = &expansion.texts[0];
        assert!(!expanded.contains("macro square"), "{expanded}");
        assert!(expanded.contains("print(((6) * (6)))"), "{expanded}");
        // The line the macro occupied is still a line.
        assert_eq!(expanded.lines().count(), program.lines().count());
    }

    #[test]
    fn an_unknown_macro_is_reported() {
        let expansion = expand_one(
            "macro known(a: expr) { expand { a } }\n\
             function f() -> Int {\n    return missing!(1)\n}\n",
        );
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC001")),
            "{:?}",
            expansion.diagnostics
        );
    }

    #[test]
    fn a_macro_may_call_another_macro() {
        let expansion = expand_one(
            "macro double(v: expr) { expand { v + v } }\n\
             macro quad(v: expr) { expand { double!(v) + double!(v) } }\n\
             function f() -> Int {\n    return quad!(3)\n}\n",
        );
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        let expanded = &expansion.texts[0];
        assert!(!expanded.contains('!'), "{expanded}");
        assert!(expanded.contains("(3)"), "{expanded}");
    }

    #[test]
    fn a_recursive_macro_hits_the_depth_limit_rather_than_hanging() {
        let expansion = expand_one(
            "macro loopy(v: expr) { expand { loopy!(v) } }\n\
             function f() -> Int {\n    return loopy!(1)\n}\n",
        );
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC010")),
            "{:?}",
            expansion.diagnostics
        );
    }

    #[test]
    fn a_derive_macro_generates_from_reflected_fields() {
        let expansion = expand_one(
            r#"
comptime macro FieldCount {
    kind { derive }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax {
        var count: Int = 0
        for field in target.fields {
            count = count + 1
        }
        return quote {
            function countOf#{target.name}() -> Int {
                return #{count}
            }
        }
    }
}

@Derive(FieldCount)
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}
"#,
        );
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        let text = &expansion.texts[0];
        assert!(text.contains("function countOfVec3() -> Int"), "{text}");
        assert!(text.contains("return 3"), "{text}");
        assert!(!text.contains("@Derive"), "{text}");
        assert!(text.contains("struct Vec3"), "{text}");
    }

    #[test]
    fn an_enum_derive_sees_its_variants() {
        let expansion = expand_one(
            r#"
comptime macro VariantCount {
    kind { derive }
    appliesTo { enum }
    expand(target: Declaration) -> Syntax {
        var count: Int = 0
        for field in target.fields {
            count = count + 1
        }
        return quote { function variants() -> Int { return #{count} } }
    }
}

@Derive(VariantCount)
enum Color {
    Red
    Green
    Blue
}
"#,
        );
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        assert!(
            expansion.texts[0].contains("return 3"),
            "{}",
            expansion.texts[0]
        );
    }

    #[test]
    fn a_derive_on_the_wrong_declaration_kind_is_refused() {
        let expansion = expand_one(
            r#"
comptime macro OnlyStructs {
    kind { derive }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax { return quote { } }
}

@Derive(OnlyStructs)
enum Color {
    Red
}
"#,
        );
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC007")),
            "{:?}",
            expansion.diagnostics
        );
    }

    #[test]
    fn a_function_macro_splices_declarations_at_file_scope() {
        let expansion = expand_one(
            r#"
comptime macro bits {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var fns: [Syntax] = []
        var value: Int = 1
        for name in names {
            fns.append(quote {
                function #{name}() -> Int { return #{value} }
            })
            value = value * 2
        }
        return quote { #{fns} }
    }
}

bits!(Read, Write, Exec)
"#,
        );
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        let text = &expansion.texts[0];
        assert!(
            text.contains("function Read() -> Int { return 1 }"),
            "{text}"
        );
        assert!(
            text.contains("function Write() -> Int { return 2 }"),
            "{text}"
        );
        assert!(
            text.contains("function Exec() -> Int { return 4 }"),
            "{text}"
        );
    }

    #[test]
    fn a_splice_glues_to_the_text_beside_it() {
        let expansion = expand_one(
            r#"
comptime macro prefixed {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let names: [Identifier] = input.identifiers()
        var fns: [Syntax] = []
        for name in names {
            fns.append(quote { function mxp_#{name}() -> Int { return 1 } })
        }
        return quote { #{fns} }
    }
}

prefixed!(Foo, Bar)
"#,
        );
        let text = &expansion.texts[0];
        assert!(text.contains("function mxp_Foo()"), "{text}");
        assert!(text.contains("function mxp_Bar()"), "{text}");
    }

    #[test]
    fn a_macro_reported_error_stops_its_expansion() {
        let expansion = expand_one(
            r#"
comptime macro NeedsWrapped {
    kind { attribute }
    appliesTo { struct }
    expand(target: Declaration) -> Syntax {
        var found: Bool = false
        for field in target.fields {
            if field.name.asString() == "wrappedValue" {
                found = true
            }
        }
        if found == false {
            Diagnostics.error("NeedsWrapped requires a wrappedValue field", at: target.syntax)
            return quote { }
        }
        return quote { function ok() -> Bool { return true } }
    }
}

@NeedsWrapped
struct Plain {
    var other: Int
}
"#,
        );
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC021")),
            "{:?}",
            expansion.diagnostics
        );
        assert!(
            !expansion.texts[0].contains("function ok()"),
            "{}",
            expansion.texts[0]
        );
    }

    #[test]
    fn a_macro_declared_in_one_file_expands_in_another() {
        let expansion = expand(&[
            (
                SourceId::new(0),
                "macro square(v: expr) { expand { v * v } }\n",
            ),
            (
                SourceId::new(1),
                "function f() -> Int {\n    return square!(5)\n}\n",
            ),
        ]);
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        assert!(
            expansion.texts[1].contains("((5) * (5))"),
            "{:?}",
            expansion.texts
        );
    }

    /// A pipeline stand-in, so the seam can be proven without one.
    struct OneShader;

    impl ShaderCompiler for OneShader {
        fn compile(&self, path: &str, target: &str) -> Result<CompiledShader, ShaderCompileError> {
            Ok(CompiledShader {
                combined_source: format!("// {target} of {path}\nvertex void v() {{}}"),
                vertex_entry: "v".to_owned(),
                fragment_entry: "f".to_owned(),
                ..CompiledShader::default()
            })
        }
    }

    /// The userland `ksl` the KSL migration is aiming at.
    ///
    /// Note what is *not* in the compiler here: `KslArtifact`, its field names,
    /// and how many backends get inlined are all Kira source.
    const USERLAND_KSL: &str = r#"
comptime macro ksl {
    kind { function }
    expand(input: Syntax) -> Syntax {
        let msl = Ksl.compile(input, "msl")
        return quote {
            KslArtifact(combinedMsl: #{msl.combinedSource}, vertexEntry: #{msl.vertexEntry})
        }
    }
}

function load() -> KslArtifact {
    return ksl!("Shaders/Tri.ksl")
}
"#;

    #[test]
    fn there_is_no_builtin_ksl_left_to_fall_back_on() {
        // `ksl!` was a compiler builtin and is not one any more: the engine
        // declares it. An undeclared call is an unknown macro like any other,
        // and if a builtin ever returned this would report something else.
        let expansion = expand_one(
            "macro other(v: expr) { expand { v } }\n\
             function f() {\n    let s = ksl!(\"Shaders/Tri.ksl\")\n}\n",
        );
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC001")),
            "{:?}",
            expansion.diagnostics
        );
    }

    #[test]
    fn with_no_pipeline_the_userland_macro_refuses_under_the_shader_code() {
        let expansion = expand_one(USERLAND_KSL);
        assert!(
            expansion
                .diagnostics
                .iter()
                .any(|d| d.code == Some("KMAC022")),
            "{:?}",
            expansion.diagnostics
        );
    }

    #[test]
    fn a_userland_ksl_macro_inlines_what_the_pipeline_compiled() {
        let shaders = OneShader;
        let expansion = expand_with(&[(SourceId::new(0), USERLAND_KSL)], Some(&shaders), "macos");
        assert!(
            expansion.diagnostics.is_empty(),
            "{:?}",
            expansion.diagnostics
        );
        let text = &expansion.texts[0];
        // The shader source crossed into generated Kira as a string literal,
        // newlines escaped — which is what makes inlining a whole backend's
        // output into an artifact work at all.
        assert!(
            text.contains(r#"combinedMsl: "// msl of Shaders/Tri.ksl\nvertex void v() {}""#),
            "{text}"
        );
        assert!(text.contains(r#"vertexEntry: "v""#), "{text}");
        assert!(!text.contains("ksl!"), "{text}");
    }
}
