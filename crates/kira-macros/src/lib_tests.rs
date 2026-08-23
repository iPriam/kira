//! Expansion tests for the whole macro pipeline.
//!
//! Split from `lib.rs` rather than trimmed: the module is the pipeline's only
//! end-to-end coverage — a program in, expanded text out — and every case is one
//! shape a macro can take. Keeping them beside the code put that file past the
//! size the repository allows.

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
fn a_comptime_function_call_becomes_its_value() {
    let program = "comptime function twice(n: Int) -> Int { return n * 2 }
                   @Main function main() {
print(twice(21))
return
}
";
    let expansion = expand_one(program);
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(!expanded.contains("comptime function"), "{expanded}");
    assert!(expanded.contains("print(42)"), "{expanded}");
    // The declaration was blanked rather than removed, so every later
    // offset is where it was.
    assert_eq!(expanded.lines().count(), program.lines().count());
}

#[test]
fn a_comptime_function_runs_statements_not_just_one_expression() {
    // The whole point of building this on the macro evaluator: locals and a
    // loop, which a single-expression folder cannot do.
    let expansion = expand_one(
        "comptime function sumTo(limit: Int) -> Int {
             var total = 0
             var i = 1
             while i <= limit {
                 total = total + i
                 i = i + 1
             }
             return total
         }
         @Main function main() {
print(sumTo(100))
return
}
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("print(5050)"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn one_comptime_function_calls_another() {
    let expansion = expand_one(
        "comptime function double(n: Int) -> Int { return n * 2 }
         comptime function quad(n: Int) -> Int { return double(double(n)) }
         @Main function main() {
print(quad(5))
return
}
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("print(20)"),
        "{}",
        expansion.texts[0]
    );
}

#[test]
fn a_method_sharing_a_comptime_functions_name_is_left_alone() {
    // A call site is found by name, so a method or a declaration wearing the
    // same name has to be told apart by what precedes it.
    let expansion = expand_one(
        "comptime function double(n: Int) -> Int { return n * 2 }
         struct Counter {
             var value: Int = 0
             function double(n: Int) -> Int { return n + self.value }
         }
         @Main function main() {
             var c = Counter { value: 100 }
             print(c.double(1))
             return
         }
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(
        expanded.contains("function double(n: Int) -> Int { return n + self.value }"),
        "{expanded}"
    );
    assert!(expanded.contains("c.double(1)"), "{expanded}");
}

#[test]
fn a_comptime_function_that_cannot_fold_is_refused_rather_than_emitted() {
    // A call left standing would reach a backend as a call to a function
    // that is not there, so an argument the evaluator cannot read is an
    // error rather than a passthrough.
    let expansion = expand_one(
        "comptime function twice(n: Int) -> Int { return n * 2 }
         @Main function main() {
var runtime = 5
print(twice(runtime))
return
}
",
    );
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code(diagnostics::UNSUPPORTED_IN_EXPAND)),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_comptime_function_that_calls_itself_is_refused_rather_than_hanging() {
    let expansion = expand_one(
        "comptime function loops(n: Int) -> Int { return loops(n) }
         @Main function main() {
print(loops(1))
return
}
",
    );
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code(diagnostics::DEPTH_LIMIT)),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn a_macro_can_give_a_family_a_lifecycle_it_did_not_write() {
    // The runtime's half of a contract the author opted into by annotating,
    // added to the family itself rather than through `extend` — so the whole
    // of the family is still one declaration a reader sees in one place.
    let expansion = expand_one(
        "comptime macro Driven {
             kind { attribute }
             appliesTo { construct }
             replace { true }
             expand(target: Declaration) -> Syntax {
                 return target.syntax.addMember(quote { lifecycle { onStart() { return } } })
             }
         }
         @Driven
         construct Task {
             @Required function label() -> String
         }
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    let expanded = &expansion.texts[0];
    assert!(
        expanded.contains("lifecycle { onStart() { return } }"),
        "{expanded}"
    );
    // Everything the family already said survives the edit.
    assert!(
        expanded.contains("@Required function label() -> String"),
        "{expanded}"
    );
}

/// A macro body names a case of an enum the *program* declares.
///
/// The evaluator has no enum of its own to offer here: `ShaderBackend` is
/// ordinary Kira, and what makes the case usable at compile time is that the
/// scan reads the program's declarations.
#[test]
fn a_macro_body_names_a_case_of_a_program_enum() {
    let expansion = expand_one(
        "enum Backend { Msl Glsl }
         comptime macro pick {
             kind { function }
             expand(input: Syntax) -> Syntax {
                 match Backend.Glsl {
                     Msl -> { return quote { \"metal\" } }
                     Glsl -> { return quote { \"opengl\" } }
                 }
             }
         }
         @Main
         function main() { print(pick!(0)) }
",
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion
            .texts
            .iter()
            .any(|text| text.contains("\"opengl\"")),
        "{:?}",
        expansion.texts
    );
}

/// A case the enum does not have is refused, and the refusal lists the ones
/// it does — which is the whole reason to write a case over a string.
#[test]
fn a_case_a_program_enum_lacks_is_refused_by_name() {
    let expansion = expand_one(
        "enum Backend { Msl Glsl }
         comptime macro pick {
             kind { function }
             expand(input: Syntax) -> Syntax {
                 let chosen = Backend.Gsl
                 return quote { 0 }
             }
         }
         @Main
         function main() { print(pick!(0)) }
",
    );
    let said = expansion
        .diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join(
            "
",
        );
    assert!(said.contains("has no case `Gsl`"), "{said}");
    assert!(said.contains("`.Msl`"), "{said}");
}

#[test]
fn a_declaration_cannot_write_a_hook_its_macro_adds() {
    // One of the two would silently win. Caught where both halves are in
    // hand, so the message can say the other one came from a macro — which
    // is the half a reader cannot see in their own source.
    let expansion = expand_one(
        "comptime macro Driven {
             kind { attribute }
             appliesTo { construct }
             replace { true }
             expand(target: Declaration) -> Syntax {
                 return target.syntax.addMember(quote { lifecycle { onStart() { return } } })
             }
         }
         @Driven
         construct Task {
             lifecycle { onStart() { return } }
         }
",
    );
    assert!(
        expansion
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code(diagnostics::NO_SUCH_FIELD)),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn an_unknown_macro_is_reported() {
    let expansion = expand_one(
        "macro known(a: expr) { expand { a } }\n\
         function f() -> Int {\n    return missing!(1)\n}\n",
    );
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC001")),
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
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC010")),
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
fn a_macro_body_searches_text_the_way_a_program_does() {
    // The lint case this exists for: a macro looking at a declaration's own
    // source and reporting what it finds there. Before the string surface
    // landed, none of these calls existed and a text-pattern lint could not
    // be written at all.
    let expansion = expand_one(
        r#"
comptime macro Inspects {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    var found: Int = 0
    if target.syntax.contains("var count") {
        found = found + 1
    }
    if target.syntax.startsWith("struct") {
        found = found + 10
    }
    if "  padded  ".trim() == "padded" {
        found = found + 100
    }
    if "A-B".lowercase() == "a-b" {
        found = found + 1000
    }
    for piece in "a,b,c".split(",") {
        found = found + 10000
    }
    return quote { function found() -> Int { return #{found} } }
}
}

@Derive(Inspects)
struct Counter {
var count: Int
}
"#,
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("return 31111"),
        "{}",
        expansion.texts[0]
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
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC007")),
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
enum ShaderBackend { Msl Wgsl Glsl Hlsl Spirv }

comptime macro ksl {
kind { function }
expand(input: Syntax) -> Syntax {
    let msl = Ksl.compile(input, ShaderBackend.Msl)
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
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC001")),
        "{:?}",
        expansion.diagnostics
    );
}

#[test]
fn with_no_pipeline_the_userland_macro_refuses_under_the_shader_code() {
    let expansion = expand_one(USERLAND_KSL);
    assert!(
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC022")),
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
