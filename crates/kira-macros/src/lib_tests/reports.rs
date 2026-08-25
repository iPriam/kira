//! How reports surface: anchors, codes, severities, and what survives them.

use super::*;
#[test]
fn a_reported_problem_points_at_what_the_macro_named() {
    let program = r#"
comptime macro Refuses {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    Diagnostics.error("this type is not supported", at: target.syntax)
    return quote { }
}
}

@Derive(Refuses)
struct Point {
var x: Int
}
"#;
    let expansion = expand_one(program);
    let reported = expansion
        .diagnostics
        .iter()
        .find(|d| d.has_code("KMAC021"))
        .unwrap_or_else(|| panic!("a reported problem: {:?}", expansion.diagnostics));
    let span = reported
        .primary_span()
        .unwrap_or_else(|| panic!("a span to point at"));
    let underlined = &program[span.span.start as usize..][..span.span.len as usize];
    // The caret lands on the struct the macro complained about, not on the
    // `comptime macro` that did the complaining.
    assert!(
        underlined.starts_with("struct Point"),
        "underlined `{underlined}`"
    );
}

#[test]
fn a_lint_reports_under_its_own_code_at_the_code_it_judged() {
    // The whole `kira lint` shape in miniature: a `Lint` entry configures a
    // check, a collector finds both the entry and the code, and the report
    // lands on the judged declaration under the lint's own code.
    let program = r#"
construct Lint {
@Required let enabled: Bool
}

comptime macro LintRunner {
kind { collector }
expand(declarations: [Declaration]) -> Syntax {
    var on: Bool = false
    for entry in declarations {
        if entry.family == "Lint" {
            for member in entry.fields {
                if member.initializer == "true" {
                    on = true
                }
            }
        }
    }
    if on {
        for target in declarations {
            if target.syntax.contains("Colour.") {
                Diagnostics.warning("qualified variant", at: target.syntax, code: "KLINT001")
            }
        }
    }
    return quote { }
}
}

construct QualifiedVariant() extends Lint {
let enabled = true
}

enum Colour {
Red
}

struct Holder {
var picked: Int = 0
}

function pick() -> Int {
let chosen = Colour.Red
return 1
}
"#;
    let expansion = expand_one(program);
    let reported = expansion
        .diagnostics
        .iter()
        .find(|d| d.has_code("KLINT001"))
        .unwrap_or_else(|| panic!("a lint report: {:?}", expansion.diagnostics));
    assert_eq!(reported.severity, kira_diagnostics::Severity::Warning);
    let span = reported
        .primary_span()
        .unwrap_or_else(|| panic!("a span to point at"));
    let underlined = &program[span.span.start as usize..][..span.span.len as usize];
    assert!(
        underlined.starts_with("function pick()"),
        "underlined `{underlined}`"
    );
}

#[test]
fn a_lint_can_walk_a_body_and_point_at_one_statement() {
    // `manual-index-loop`, the lint this surface exists for: a `while`
    // comparing a counter, whose last statement steps that same counter by
    // one. Before the body surface this was unwritable — a token scan can
    // see the text but not which statement is last.
    let program = r#"
comptime macro LintRunner {
kind { collector }
expand(declarations: [Declaration]) -> Syntax {
    for target in declarations {
        for statement in target.body {
            if statement.kind == "while" {
                if statement.head.contains(".count") {
                    var last: String = ""
                    for inner in statement.body {
                        last = inner.syntax
                    }
                    if last.contains(" + 1") {
                        Diagnostics.warning(
                            "this `while` counts an index by hand; `for` does it for you",
                            at: statement.syntax,
                            code: "KLINT002"
                        )
                    }
                }
            }
        }
    }
    return quote { }
}
}

function total(xs: [Int]) -> Int {
var sum = 0
var index = 0
while index < xs.count {
    sum = sum + xs[index]
    index = index + 1
}
return sum
}
"#;
    let expansion = expand_one(program);
    let reported = expansion
        .diagnostics
        .iter()
        .find(|d| d.has_code("KLINT002"))
        .unwrap_or_else(|| panic!("a lint report: {:?}", expansion.diagnostics));
    let span = reported
        .primary_span()
        .unwrap_or_else(|| panic!("a span to point at"));
    let underlined = &program[span.span.start as usize..][..span.span.len as usize];
    // The caret lands on the loop, not on the whole function — which is the
    // whole point of walking the body rather than the declaration.
    assert!(
        underlined.starts_with("while index < xs.count"),
        "underlined `{underlined}`"
    );
}

#[test]
fn a_warning_reports_without_discarding_what_the_macro_built() {
    let program = r#"
comptime macro Observes {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    Diagnostics.warning("this type could be simpler", at: target.syntax)
    return quote { function observed() -> Int { return 1 } }
}
}

@Derive(Observes)
struct Point {
var x: Int
}
"#;
    let expansion = expand_one(program);
    let reported = expansion
        .diagnostics
        .iter()
        .find(|d| d.has_code("KMAC021"))
        .unwrap_or_else(|| panic!("a reported problem: {:?}", expansion.diagnostics));
    assert_eq!(reported.severity, kira_diagnostics::Severity::Warning);
    // The whole point of a warning: the macro had an opinion, not an
    // objection, so what it generated is still there.
    assert!(
        expansion.texts[0].contains("function observed()"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn an_error_still_discards_what_the_macro_built() {
    let program = r#"
comptime macro Refuses {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    Diagnostics.error("this type is not supported", at: target.syntax)
    return quote { function generated() -> Int { return 1 } }
}
}

@Derive(Refuses)
struct Point {
var x: Int
}
"#;
    let expansion = expand_one(program);
    assert!(
        !expansion.texts[0].contains("function generated()"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn a_macro_that_names_nothing_still_points_at_itself() {
    let program = r#"
comptime macro Bare {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    Diagnostics.error("no anchor given")
    return quote { }
}
}

@Derive(Bare)
struct Point {
var x: Int
}
"#;
    let expansion = expand_one(program);
    // Reported without an `at:`, so it falls back to the annotation that
    // summoned the macro rather than losing the diagnostic altogether.
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|d| d.has_code("KMAC021") && d.primary_span().is_some()),
        "{:?}",
        expansion.diagnostics
    );
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
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC021")),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        !expansion.texts[0].contains("function ok()"),
        "{}",
        expansion.texts[0]
    );
}
