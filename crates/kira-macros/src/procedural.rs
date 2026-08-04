//! Driving the procedural macros: which macros a declaration summons, in what
//! order, and what happens to the declaration afterwards.
//!
//! The ordering rule is the load-bearing one. **Every attribute and derive
//! macro observes the original declaration** — no macro ever sees another
//! macro's output — and the outputs are concatenated with the original in the
//! source order of the annotations. Because no macro sees another's output,
//! sibling-generated blocks can never form an ordering dependency on each
//! other.

use std::collections::HashMap;

use kira_diagnostics::{Diagnostic, Severity};
use kira_source::Span;
use kira_syntax_model::TokenKind;

use crate::decl::{self, Declaration};
use crate::diagnostics::{self, Reporter};
use crate::edits::EditBuffer;
use crate::eval::{self, methods};
use crate::invoke::{Invocation, Position};
use crate::ksl::ShaderCompiler;
use crate::registry::{Procedural, ProceduralKind, Registry, kind_word};
use crate::tokens::Lexed;
use crate::value::{DeclarationValue, Value};

/// The derive name the compiler owns rather than a macro.
///
/// `@Derive(Copy)` generates nothing: it is an opt-in assertion that a type is
/// structurally copyable, checked during IR lowering. Expansion leaves it in
/// place and never treats it as a missing macro.
pub(crate) const BUILTIN_DERIVE_COPY: &str = "Copy";

/// What every file in one expansion shares: the macros in scope, the wrapper
/// templates they registered, and the KSL pipeline their bodies can reach.
///
/// Grouped rather than passed one by one because all three travel together
/// down the whole call chain and none of them changes on the way.
#[derive(Clone, Copy)]
pub(crate) struct Program<'a> {
    /// Every macro the program declares.
    pub(crate) registry: &'a Registry,
    /// Every struct registered as a wrapper template.
    pub(crate) templates: &'a HashMap<String, WrapperTemplate>,
    /// The KSL pipeline behind the `Ksl` namespace, when one was supplied.
    pub(crate) shaders: Option<&'a dyn ShaderCompiler>,
    /// The operating system this build targets, behind `Build.platform`.
    pub(crate) platform: &'a str,
}

/// A struct registered as a wrapper template by a `kind { wrapper }` macro.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct WrapperTemplate {
    /// The macro that registered it.
    pub(crate) macro_name: String,
    /// The template declaration itself.
    pub(crate) declaration: Declaration,
}

/// Finds every wrapper template in the program.
///
/// A pre-scan, so declaration order between files and packages never matters: a
/// field may name a wrapper declared in a file parsed after its own.
pub(crate) fn wrapper_templates<'a>(
    files: impl Iterator<Item = &'a [Declaration]>,
    registry: &Registry,
) -> HashMap<String, WrapperTemplate> {
    let mut templates = HashMap::new();
    for declarations in files {
        for declaration in declarations {
            for annotation in &declaration.annotations {
                let Some(declared) = registry.procedural(&annotation.name) else {
                    continue;
                };
                if declared.kind != ProceduralKind::Wrapper {
                    continue;
                }
                templates.insert(
                    declaration.name.clone(),
                    WrapperTemplate {
                        macro_name: declared.name.clone(),
                        declaration: declaration.clone(),
                    },
                );
            }
        }
    }
    templates
}

/// Every top-level declaration in `file`, annotations included.
pub(crate) fn top_level(file: &Lexed<'_>) -> Vec<Declaration> {
    let mut found = Vec::new();
    let mut index = 0usize;
    while index < file.len() {
        match file.kind(index) {
            TokenKind::Eof => break,
            TokenKind::At
            | TokenKind::Struct
            | TokenKind::Class
            | TokenKind::Enum
            | TokenKind::Construct
            | TokenKind::Function => {}
            // A construct-backed declaration, in both spellings: `Family
            // Name(params) {` and the parameterless `Family Name {`.
            TokenKind::Identifier
                if file.is_ident(index + 1)
                    && matches!(file.kind(index + 2), TokenKind::LParen | TokenKind::LBrace) => {}
            TokenKind::LBrace | TokenKind::LParen | TokenKind::LBracket => {
                match file.match_close(index) {
                    Some(close) => index = close + 1,
                    None => break,
                }
                continue;
            }
            _ => {
                index += 1;
                continue;
            }
        }
        match decl::scan(file, index) {
            Some((declaration, next)) => {
                found.push(declaration);
                index = next.max(index + 1);
            }
            None => break,
        }
    }
    found
}

/// Runs every `collector` macro over the program's declarations.
///
/// Returns one appended file per collector that produced output, plus whatever
/// the collectors reported. A program with no collector does no work here and
/// appends nothing.
pub(crate) fn collect<'a>(
    registry: &Registry,
    declarations: impl Iterator<Item = &'a Declaration>,
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
    lint: bool,
) -> (Vec<String>, Vec<Diagnostic>) {
    let collectors = registry.of_kind(ProceduralKind::Collector);
    if collectors.is_empty() {
        return (Vec::new(), Vec::new());
    }
    let all = Value::Array(
        declarations
            .map(|declaration| Value::Declaration(Box::new(DeclarationValue::of(declaration))))
            .collect(),
    );
    let mut appended = Vec::new();
    let mut reporter = Reporter::default();
    for declared in collectors {
        let parameter = parameter(declared, 0);
        let Some(body) = eval::compile(&declared.body) else {
            reporter.error(
                declared.source,
                declared.span,
                diagnostics::EXPAND_SIGNATURE,
                format!("the `expand` body of `{}` does not parse", declared.name),
            );
            continue;
        };
        match eval::run(
            &body,
            vec![(parameter, all.clone())],
            shaders,
            platform,
            lint,
        ) {
            Ok(outcome) => {
                // Only an error discards what the collector built. A collector
                // that warns — which is what a lint does — still gets its
                // output appended, because nothing it said was fatal.
                let failed = outcome
                    .reported
                    .iter()
                    .any(|report| report.severity == Severity::Error);
                for report in outcome.reported {
                    // A collector runs over the whole program, so what it is
                    // talking about is almost never itself. Point at the
                    // declaration it named, and fall back to the collector only
                    // when it named nothing that came from a file.
                    let (source, span) = report
                        .at
                        .map_or((declared.source, declared.span), |at| (at.source, at.span));
                    // A lint names itself and is suppressed by that name, so
                    // its own code reaches the diagnostic; a macro that named
                    // none reports under the shared one.
                    reporter.coded(
                        report.severity,
                        source,
                        span,
                        report.code.as_deref(),
                        report.fix.as_deref(),
                        report.message,
                    );
                }
                if !failed && !outcome.syntax.trim().is_empty() {
                    appended.push(outcome.syntax);
                }
            }
            Err(error) => reporter.error(declared.source, declared.span, error.code, error.message),
        }
    }
    (appended, reporter.into_diagnostics())
}

/// Runs every macro one declaration summons, recording the edits it needs.
pub(crate) fn expand_declaration(
    file: &Lexed<'_>,
    declaration: &Declaration,
    program: Program<'_>,
    buffer: &mut EditBuffer,
    reporter: &mut Reporter,
) {
    let Program {
        registry,
        shaders,
        platform,
        ..
    } = program;
    let mut generated: Vec<String> = Vec::new();
    let mut replacement: Option<String> = None;
    let mut consumed: Vec<Span> = Vec::new();
    let mut is_template = false;

    for annotation in &declaration.annotations {
        if annotation.name == "Derive" {
            let kept = run_derives(
                file,
                declaration,
                annotation,
                program,
                &mut generated,
                reporter,
            );
            match kept {
                Some(remaining) => buffer.replace(annotation.span, remaining),
                None => consumed.push(annotation.span),
            }
            continue;
        }
        let Some(declared) = registry.procedural(&annotation.name) else {
            continue;
        };
        match declared.kind {
            // A collector is not summoned by an annotation: it runs once for
            // the whole program, from `collect`. Reaching here means a program
            // wrote `@Name` with a collector's name, which is not a site it
            // has.
            ProceduralKind::Collector => continue,
            ProceduralKind::Attribute => {
                if !check_applies_to(file, declaration, declared, annotation.span, reporter) {
                    consumed.push(annotation.span);
                    continue;
                }
                if let Some(output) = run(
                    file,
                    declared,
                    vec![(
                        parameter(declared, 0),
                        methods::declaration_value(declaration),
                    )],
                    annotation.span,
                    shaders,
                    platform,
                    reporter,
                ) {
                    if declared.replace {
                        if replacement.is_some() {
                            reporter.error(
                                file.source,
                                annotation.span,
                                diagnostics::TWO_REPLACERS,
                                format!(
                                    "`{}` replaces `{}`, but another macro already did: a second \
                                     replacer would have no original left to observe",
                                    declared.name, declaration.name
                                ),
                            );
                        } else {
                            replacement = Some(output);
                        }
                    } else {
                        generated.push(output);
                    }
                }
                consumed.push(annotation.span);
            }
            ProceduralKind::Wrapper => {
                // Annotating a struct with a wrapper macro's name registers it
                // as a template and runs the macro's validation invocation,
                // `expand(template, template)`. The template itself never
                // reaches the compiler: it may carry placeholder types.
                is_template = true;
                let value = methods::declaration_value(declaration);
                if let Some(output) = run(
                    file,
                    declared,
                    vec![
                        (parameter(declared, 0), value.clone()),
                        (parameter(declared, 1), value),
                    ],
                    annotation.span,
                    shaders,
                    platform,
                    reporter,
                ) {
                    generated.push(output);
                }
                consumed.push(annotation.span);
            }
            ProceduralKind::Derive => {
                reporter.error(
                    file.source,
                    annotation.span,
                    diagnostics::NOT_A_DERIVE,
                    format!(
                        "`{}` is a `derive` macro, so it is written `@Derive({})`",
                        declared.name, declared.name
                    ),
                );
                consumed.push(annotation.span);
            }
            ProceduralKind::Function => {
                reporter.error(
                    file.source,
                    annotation.span,
                    diagnostics::NOT_A_DERIVE,
                    format!(
                        "`{}` is a `function` macro, so it is called `{}!(…)` rather than written \
                         as an annotation",
                        declared.name, declared.name
                    ),
                );
                consumed.push(annotation.span);
            }
        }
    }

    if !is_template
        && let Some(summoned) =
            summon_from_fields(file, declaration, program, &mut generated, reporter)
    {
        match replacement {
            Some(_) => reporter.error(
                file.source,
                declaration.span,
                diagnostics::TWO_REPLACERS,
                format!(
                    "`{}` is rewritten by more than one macro: a second replacer would have no \
                     original left to observe",
                    declaration.name
                ),
            ),
            None => replacement = Some(summoned),
        }
    }

    for span in consumed {
        buffer.blank(span, file.text);
    }
    if is_template {
        buffer.blank(declaration.span, file.text);
    } else if let Some(text) = replacement {
        buffer.blank(declaration.span, file.text);
        buffer.append(&text);
    }
    for text in generated {
        buffer.append(&text);
    }
}

/// Runs each macro named in one `@Derive(…)`, returning the annotation text to
/// keep when some names are not macros this pass owns.
fn run_derives(
    file: &Lexed<'_>,
    declaration: &Declaration,
    annotation: &decl::Annotation,
    program: Program<'_>,
    generated: &mut Vec<String>,
    reporter: &mut Reporter,
) -> Option<String> {
    let Program {
        registry,
        shaders,
        platform,
        ..
    } = program;
    let mut kept: Vec<&str> = Vec::new();
    for name in &annotation.arguments {
        if name == BUILTIN_DERIVE_COPY {
            kept.push(name);
            continue;
        }
        let Some(declared) = registry.procedural(name) else {
            reporter.error(
                file.source,
                annotation.span,
                diagnostics::NOT_A_DERIVE,
                format!("`{name}` is not a `derive` macro"),
            );
            continue;
        };
        if declared.kind != ProceduralKind::Derive {
            reporter.error(
                file.source,
                annotation.span,
                diagnostics::NOT_A_DERIVE,
                format!(
                    "`{name}` is a `{}` macro, so it cannot be derived",
                    kind_word(declared.kind)
                ),
            );
            continue;
        }
        if !check_applies_to(file, declaration, declared, annotation.span, reporter) {
            continue;
        }
        if let Some(output) = run(
            file,
            declared,
            vec![(
                parameter(declared, 0),
                methods::declaration_value(declaration),
            )],
            annotation.span,
            shaders,
            platform,
            reporter,
        ) {
            generated.push(output);
        }
    }
    if kept.is_empty() {
        return None;
    }
    let rebuilt = format!("@Derive({})", kept.join(", "));
    let width = crate::edits::slice_of(file.text, annotation.span).len();
    Some(format!("{rebuilt:<width$}"))
}

/// Runs the macro a field annotation summons over the whole declaration.
fn summon_from_fields(
    file: &Lexed<'_>,
    declaration: &Declaration,
    program: Program<'_>,
    generated: &mut Vec<String>,
    reporter: &mut Reporter,
) -> Option<String> {
    let Program {
        registry,
        templates,
        shaders,
        platform,
    } = program;
    let mut summoned: Option<String> = None;
    let mut already: Vec<String> = Vec::new();
    for field in &declaration.fields {
        for annotation in &field.annotations {
            if already.contains(&annotation.name) {
                continue;
            }
            if let Some(template) = templates.get(&annotation.name) {
                let Some(declared) = registry.procedural(&template.macro_name) else {
                    continue;
                };
                already.push(annotation.name.clone());
                let output = run(
                    file,
                    declared,
                    vec![
                        (
                            parameter(declared, 0),
                            methods::declaration_value(declaration),
                        ),
                        (
                            parameter(declared, 1),
                            methods::declaration_value(&template.declaration),
                        ),
                    ],
                    annotation.span,
                    shaders,
                    platform,
                    reporter,
                );
                summoned = replace_once(summoned, output, file, declaration, reporter);
                continue;
            }
            let Some(declared) = registry.procedural(&annotation.name) else {
                continue;
            };
            if !declared.trigger_field {
                continue;
            }
            already.push(annotation.name.clone());
            if !check_applies_to(file, declaration, declared, annotation.span, reporter) {
                continue;
            }
            let output = run(
                file,
                declared,
                vec![(
                    parameter(declared, 0),
                    methods::declaration_value(declaration),
                )],
                annotation.span,
                shaders,
                platform,
                reporter,
            );
            if declared.replace {
                summoned = replace_once(summoned, output, file, declaration, reporter);
            } else if let Some(text) = output {
                generated.push(text);
            }
        }
    }
    summoned
}

/// Keeps the first replacement and reports a second.
fn replace_once(
    current: Option<String>,
    output: Option<String>,
    file: &Lexed<'_>,
    declaration: &Declaration,
    reporter: &mut Reporter,
) -> Option<String> {
    match (current, output) {
        (None, output) => output,
        (Some(first), None) => Some(first),
        (Some(first), Some(_)) => {
            reporter.error(
                file.source,
                declaration.span,
                diagnostics::TWO_REPLACERS,
                format!(
                    "`{}` is rewritten by more than one macro: a second replacer would have no \
                     original left to observe",
                    declaration.name
                ),
            );
            Some(first)
        }
    }
}

/// Whether `declared` may annotate `declaration`.
fn check_applies_to(
    file: &Lexed<'_>,
    declaration: &Declaration,
    declared: &Procedural,
    span: Span,
    reporter: &mut Reporter,
) -> bool {
    let word = declaration.kind.word();
    if declared.applies_to.iter().any(|listed| listed == word) {
        return true;
    }
    reporter.error(
        file.source,
        span,
        diagnostics::APPLIES_TO,
        format!(
            "`{}` applies to {}, not to a {word}",
            declared.name,
            declared.applies_to.join(", ")
        ),
    );
    false
}

/// The name of `declared`'s `expand` parameter at `index`.
fn parameter(declared: &Procedural, index: usize) -> String {
    declared
        .parameters
        .get(index)
        .cloned()
        .unwrap_or_else(|| format!("__kmac_parameter_{index}"))
}

/// Runs one macro's `expand` body and reports whatever it raised.
pub(crate) fn run(
    file: &Lexed<'_>,
    declared: &Procedural,
    arguments: Vec<(String, Value)>,
    span: Span,
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
    reporter: &mut Reporter,
) -> Option<String> {
    let Some(body) = eval::compile(&declared.body) else {
        reporter.error(
            declared.source,
            declared.span,
            diagnostics::EXPAND_SIGNATURE,
            format!("the `expand` body of `{}` does not parse", declared.name),
        );
        return None;
    };
    // Not a collector, so not told: `Build.linting` answers which verb asked,
    // and only the macro form a verb runs for has any business asking.
    match eval::run(&body, arguments, shaders, platform, false) {
        Ok(outcome) => {
            // A warning leaves the expansion standing; only a refusal drops it.
            let failed = outcome
                .reported
                .iter()
                .any(|report| report.severity == Severity::Error);
            for report in outcome.reported {
                // `span` is the call or the annotation that summoned this macro,
                // which is already the right place for a macro that names
                // nothing — but a derive complaining about one field should
                // underline that field, not the whole `@Derive`.
                let (source, at) = report
                    .at
                    .map_or((file.source, span), |at| (at.source, at.span));
                reporter.coded(
                    report.severity,
                    source,
                    at,
                    report.code.as_deref(),
                    report.fix.as_deref(),
                    report.message,
                );
            }
            if failed { None } else { Some(outcome.syntax) }
        }
        Err(error) => {
            reporter.error(file.source, span, error.code, error.message);
            None
        }
    }
}

/// Expands one `name!(…)` call of a `function`-kind macro.
pub(crate) fn expand_call(
    file: &Lexed<'_>,
    declared: &Procedural,
    call: &Invocation,
    shaders: Option<&dyn ShaderCompiler>,
    platform: &str,
    reporter: &mut Reporter,
) -> Option<String> {
    if declared.kind != ProceduralKind::Function {
        reporter.error(
            file.source,
            call.name_span,
            diagnostics::NOT_A_DERIVE,
            format!(
                "`{}` is a `{}` macro, so it annotates a declaration rather than being called",
                declared.name,
                kind_word(declared.kind)
            ),
        );
        return None;
    }
    let input = call
        .arguments
        .iter()
        .map(|span| file.slice(*span).trim())
        .collect::<Vec<_>>()
        .join(", ");
    let output = run(
        file,
        declared,
        vec![(parameter(declared, 0), Value::built(input))],
        call.span,
        shaders,
        platform,
        reporter,
    )?;
    let trimmed = output.trim().to_owned();
    match call.position {
        Position::Declaration => {
            if crate::probe::is_declarations(&trimmed) {
                Some(trimmed)
            } else {
                reporter.error(
                    file.source,
                    call.span,
                    diagnostics::NOT_STATEMENTS,
                    format!(
                        "`{}` was called at file scope, so its expansion must be declarations",
                        declared.name
                    ),
                );
                None
            }
        }
        Position::Statement => {
            if crate::probe::is_statements(&trimmed) {
                Some(trimmed)
            } else {
                reporter.error(
                    file.source,
                    call.span,
                    diagnostics::NOT_STATEMENTS,
                    format!(
                        "`{}` was called in statement position, so its expansion must parse as \
                         statements",
                        declared.name
                    ),
                );
                None
            }
        }
        Position::Expression => {
            if crate::probe::is_expression(&trimmed) {
                Some(format!("({trimmed})"))
            } else {
                reporter.error(
                    file.source,
                    call.span,
                    diagnostics::NOT_AN_EXPRESSION,
                    format!(
                        "`{}` was used where a value is expected, so its expansion must be a \
                         single expression",
                        declared.name
                    ),
                );
                None
            }
        }
    }
}
