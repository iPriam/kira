//! What the scanner finds, and what it refuses.

use kira_diagnostics::Diagnostic;
use kira_source::SourceId;

use crate::diagnostics::Reporter;
use crate::tokens::Lexed;

use super::model::{FragmentKind, ProceduralKind};
use super::{Registry, collect_file};

fn collect_text(text: &str) -> (Registry, Vec<Diagnostic>) {
    let mut reporter = Reporter::new();
    let mut registry = Registry::default();
    registry.absorb(
        None,
        &collect_file(&Lexed::new(SourceId::new(0), text), &mut reporter),
        &mut Vec::new(),
    );
    (registry, reporter.into_diagnostics())
}

/// A name a nearer package takes over leaves every kind, not just its own.
///
/// The three kinds are three maps and the lookups are by kind, so a claim
/// that only wrote the map its own kind lives in left the shadowed
/// declaration reachable through a lookup of a different one: an
/// application's declarative `Name` would take the name while `@Name`
/// still ran a dependency's attribute macro.
#[test]
fn a_shadowed_declaration_leaves_every_kind() {
    let dependency = "comptime macro Name {\n                          kind { attribute }\n                          appliesTo { struct }\n                          replace { true }\n                          expand(declaration, arguments) { return declaration }\n                          }";
    let application = "macro Name(value: expr) {\n    expand {\n        value\n    }\n}";
    let mut reporter = Reporter::new();
    let mut registry = Registry::default();
    registry.absorb(
        Some("Dependency"),
        &collect_file(&Lexed::new(SourceId::new(0), dependency), &mut reporter),
        &mut Vec::new(),
    );
    assert!(
        registry.procedural("Name").is_some(),
        "the dependency's attribute macro is what is being shadowed"
    );
    registry.absorb(
        None,
        &collect_file(&Lexed::new(SourceId::new(1), application), &mut reporter),
        &mut Vec::new(),
    );
    assert!(
        registry.declarative("Name").is_some(),
        "the application's declaration takes the name"
    );
    assert!(
        registry.procedural("Name").is_none(),
        "the shadowed attribute macro must not still answer `@Name`"
    );
    assert!(
        registry.of_kind(ProceduralKind::Attribute).is_empty(),
        "nor be found by a sweep of its kind"
    );
}

/// A macro definition scans the same indented or not: the scanner reads
/// tokens, and tokens carry no columns.
#[test]
fn indentation_does_not_change_what_a_definition_is() {
    let indented = "comptime macro BadName {\n    kind { function }\n    expand(input: Syntax) -> Syntax {\n        return quote { 42 }\n    }\n}\n";
    let flat = "comptime macro BadName {\nkind { function }\nexpand(input: Syntax) -> Syntax {\nreturn quote { 42 }\n}\n}\n";
    for text in [indented, flat] {
        let (registry, diagnostics) = collect_text(text);
        assert!(diagnostics.is_empty(), "{text:?}: {diagnostics:?}");
        assert!(registry.procedural("BadName").is_some(), "{text:?}");
    }
}

/// The blanked span covers the whole definition, indented or not: what the
/// parser never sees cannot produce diagnostics.
#[test]
fn the_definition_span_covers_the_definition() {
    let flat = "comptime macro BadName {\nkind { function }\nexpand(input: Syntax) -> Syntax {\nreturn quote { 42 }\n}\n}\n";
    let mut reporter = Reporter::new();
    let found = collect_file(&Lexed::new(SourceId::new(0), flat), &mut reporter);
    assert!(reporter.into_diagnostics().is_empty());
    assert_eq!(found.spans.len(), 1);
    let covered = &flat[found.spans[0].start as usize..found.spans[0].end() as usize];
    assert!(covered.contains("return quote { 42 }"), "{covered:?}");
}

/// …and nothing else: a span running past the definition would blank the
/// `@Main` below it, and the program would lose its entrypoint.
#[test]
fn the_definition_span_stops_at_the_definition() {
    let flat = "comptime macro BadName {\nkind { function }\nexpand(input: Syntax) -> Syntax {\nreturn quote { 42 }\n}\n}\n@Main\nfunction main() {\nprint(1)\nreturn\n}\n";
    let mut reporter = Reporter::new();
    let found = collect_file(&Lexed::new(SourceId::new(0), flat), &mut reporter);
    assert!(reporter.into_diagnostics().is_empty());
    assert_eq!(found.spans.len(), 1);
    let end = found.spans[0].end() as usize;
    assert!(flat[end..].contains("@Main"), "tail: {:?}", &flat[end..]);
}

#[test]
fn a_declarative_macro_registers_its_fragments_and_template() {
    let (registry, diagnostics) = collect_text(
        "macro square(value: expr) {\n    expand {\n        value * value\n    }\n}\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let declared = registry.declarative("square").expect("the macro");
    assert_eq!(declared.fragments.len(), 1);
    assert_eq!(declared.fragments[0].kind, FragmentKind::Expr);
    assert!(declared.template.contains("value * value"));
}

/// An unclosed macro body blanks the file from the breakage on: the
/// definition's raw `quote` text must never reach the parser, or every
/// surviving `#{` is reported as an unexpected character burying the scan
/// error that names the actual failure.
#[test]
fn an_unclosed_macro_body_blanks_its_tail() {
    let text = "comptime macro Broken {\n    kind { derive }\n    expand(target: Declaration) -> Syntax {\n        return quote { x }\n";
    let mut reporter = Reporter::new();
    let found = collect_file(&Lexed::new(SourceId::new(0), text), &mut reporter);
    let diagnostics = reporter.into_diagnostics();
    assert!(
        diagnostics.iter().any(|d| d.message.contains("unclosed")),
        "{diagnostics:?}"
    );
    assert!(
        !found.spans.is_empty(),
        "the unscannable tail must be blanked"
    );
    let last = found.spans.last().expect("a span");
    assert_eq!(last.end() as usize, text.len());
}

#[test]
fn a_place_fragment_is_recognized() {
    let (registry, _) = collect_text("macro swap(a: place, b: place) { expand { a = b } }");
    let declared = registry.declarative("swap").expect("the macro");
    assert_eq!(declared.fragments.len(), 2);
    assert!(
        declared
            .fragments
            .iter()
            .all(|fragment| fragment.kind == FragmentKind::Place)
    );
}

#[test]
fn a_procedural_macro_records_every_member() {
    let (registry, diagnostics) = collect_text(
        "comptime macro Tracked {\n    kind { attribute }\n    appliesTo { form }\n\
         \n    trigger { field }\n    replace { true }\n\
         \n    expand(target: Declaration) -> Syntax {\n        return quote { }\n    }\n}\n",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let declared = registry.procedural("Tracked").expect("the macro");
    assert_eq!(declared.kind, ProceduralKind::Attribute);
    assert_eq!(declared.applies_to, vec!["form".to_owned()]);
    assert!(declared.trigger_field);
    assert!(declared.replace);
    assert_eq!(declared.parameters, vec!["target".to_owned()]);
}

#[test]
fn a_field_trigger_without_replace_is_refused() {
    let (_, diagnostics) = collect_text(
        "comptime macro T {\n    kind { attribute }\n    appliesTo { form }\n\
         trigger { field }\n    expand(t: Declaration) -> Syntax { return quote { } }\n}\n",
    );
    assert!(
        diagnostics.iter().any(|d| d.has_code("KMAC029")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_missing_kind_is_refused() {
    let (registry, diagnostics) = collect_text(
        "comptime macro T {\n    expand(t: Declaration) -> Syntax { return quote { } }\n}\n",
    );
    assert!(registry.procedural("T").is_none());
    assert!(
        diagnostics.iter().any(|d| d.has_code("KMAC006")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_function_macro_may_not_declare_applies_to() {
    let (_, diagnostics) = collect_text(
        "comptime macro bits {\n    kind { function }\n    appliesTo { struct }\n\
         expand(input: Syntax) -> Syntax { return quote { } }\n}\n",
    );
    assert!(
        diagnostics.iter().any(|d| d.has_code("KMAC008")),
        "{diagnostics:?}"
    );
}

#[test]
fn a_local_named_macro_is_not_a_declaration() {
    let (registry, diagnostics) =
        collect_text("function f() {\n    let macro = 1\n    return\n}\n");
    assert!(registry.is_empty());
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}
