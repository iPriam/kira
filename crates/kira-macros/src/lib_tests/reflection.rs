//! What a macro sees: reflection over declarations, fields, enums, and text.

use super::*;
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
fn a_macro_body_slices_text_with_substring() {
    // The Serde case this exists for: an array type's element is the text
    // between its brackets, and no combination of contains, split, and trim
    // can carve a nested `[[Int]]` down to its `[Int]` without it.
    let expansion = expand_one(
        r#"
comptime macro Slices {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    var inner: String = "[Int]".substring(1, 4)
    var nested: String = "[[Int]]".substring(1, 6)
    return quote { function sliced() -> String { return #{inner} + #{nested} } }
}
}

@Derive(Slices)
struct Holder {
var items: [Int]
}
"#,
    );
    assert!(
        expansion.diagnostics.is_empty(),
        "{:?}",
        expansion.diagnostics
    );
    assert!(
        expansion.texts[0].contains("return \"Int\" + \"[Int]\""),
        "{}",
        expansion.texts[0]
    );
}

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
fn a_macro_renders_syntax_as_text_with_string() {
    // Reflection hands a macro its input as syntax; `String` renders the text
    // it was written with, which is what lets a lint join statements into one
    // searchable run instead of matching each value in place.
    let expansion = expand_one(
        r#"
comptime macro Renders {
kind { derive }
appliesTo { struct }
expand(target: Declaration) -> Syntax {
    let text: String = String(target.syntax)
    var found: Int = 0
    if text.contains("var count") {
        found = found + 1
    }
    if text.startsWith("struct") {
        found = found + 10
    }
    return quote { function rendered() -> Int { return #{found} } }
}
}

@Derive(Renders)
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
        expansion.texts[0].contains("return 11"),
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
        expansion.diagnostics.iter().any(|d| d.has_code("KMAC007")),
        "{:?}",
        expansion.diagnostics
    );
}
