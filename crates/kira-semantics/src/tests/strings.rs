//! The `String` value surface: `String(x)` and the three primitives, and what
//! each one refuses.

use super::*;

#[test]
fn the_primitives_type_check() {
    let items = diagnostics(
        "@Main function main() {\n\
         \x20   print(String(1))\n\
         \x20   print(String(true))\n\
         \x20   print(String(1.5))\n\
         \x20   print(\"abc\".count)\n\
         \x20   print(\"abc\".charAt(0))\n\
         \x20   print(\"abc\".substring(0, 2))\n\
         \x20   print(\"abc\".indexOf(\"b\"))\n\
         \x20   return\n}\n",
    );
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn each_primitive_has_its_own_type() {
    let program = analyze_text(
        "@Main function main() {\n\
         \x20   let a = \"abc\".charAt(0)\n\
         \x20   let b = \"abc\".substring(0, 1)\n\
         \x20   let c = \"abc\".indexOf(\"b\")\n\
         \x20   let d = String(1)\n\
         \x20   print(a + c)\n\
         \x20   print(b + d)\n\
         \x20   return\n}\n",
    );
    let types: Vec<Type> = program
        .functions
        .iter()
        .flat_map(|function| function.body.iter())
        .filter_map(|&stmt| match program.stmt(stmt) {
            HirStmt::Let { init, .. } => Some(program.expr(*init).type_of()),
            _ => None,
        })
        .collect();
    assert_eq!(
        types,
        vec![Type::INT, Type::String, Type::INT, Type::String],
        "{types:?}"
    );
}

#[test]
fn a_property_written_as_a_method_says_so() {
    let items = diagnostics("@Main function main() {\n    print(\"a\".count())\n    return\n}\n");
    assert!(
        items.iter().any(
            |item| item.code == Some("KSEM101") && item.message.contains("without parentheses")
        ),
        "{items:?}"
    );
}

#[test]
fn a_method_written_as_a_property_says_so() {
    let items = diagnostics("@Main function main() {\n    print(\"a\".charAt)\n    return\n}\n");
    assert!(
        items
            .iter()
            .any(|item| item.code == Some("KSEM101") && item.message.contains("is a method")),
        "{items:?}"
    );
}

#[test]
fn a_wrong_argument_type_is_refused() {
    let items =
        diagnostics("@Main function main() {\n    print(\"a\".charAt(\"b\"))\n    return\n}\n");
    assert!(
        items.iter().any(|item| item.code == Some("KSEM211")),
        "{items:?}"
    );
}

#[test]
fn a_wrong_argument_count_is_refused() {
    let items =
        diagnostics("@Main function main() {\n    print(\"a\".substring(1))\n    return\n}\n");
    assert!(
        items.iter().any(|item| item.code == Some("KSEM210")),
        "{items:?}"
    );
}

#[test]
fn an_unknown_string_member_is_refused() {
    let items = diagnostics("@Main function main() {\n    print(\"a\".nope())\n    return\n}\n");
    assert!(
        items.iter().any(|item| item.code == Some("KSEM101")),
        "{items:?}"
    );
}

#[test]
fn a_non_scalar_cannot_be_rendered_as_text() {
    let items = diagnostics(
        "struct P { var x: Int }\n\
         @Main function main() {\n    print(String(P { x: 1 }))\n    return\n}\n",
    );
    assert!(
        items.iter().any(|item| item.code == Some("KSEM209")),
        "{items:?}"
    );
}

/// A local named `String` shadows the conversion, exactly as a local shadows a
/// numeric one — `String(x)` then calls the local, and calling a non-callable
/// local is what gets reported rather than a conversion nobody wrote.
#[test]
fn a_local_shadows_the_conversion() {
    let items = diagnostics(
        "@Main function main() {\n    let String = 1\n    print(String(2))\n    return\n}\n",
    );
    assert!(
        !items.iter().any(|item| item.code == Some("KSEM209")),
        "the conversion answered for a shadowed name: {items:?}"
    );
}
