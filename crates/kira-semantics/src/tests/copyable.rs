//! `@Derive(Copy)`: eligible types compile untouched, ineligible ones are
//! `KIR005`.

use super::*;

/// The code and the offending member, so a message that stops naming what to
/// fix fails here.
fn copy_refusal(text: &str) -> String {
    let items = diagnostics(text);
    let refusal = items
        .iter()
        .find(|item| item.has_code("KIR005"))
        .unwrap_or_else(|| panic!("expected a KIR005, got {items:?}"));
    refusal.message.clone()
}

#[test]
fn a_scalar_struct_is_eligible() {
    let items = diagnostics(
        "@Derive(Copy)\nstruct Point {\n    var x: Int\n    var y: Int\n}\n\
         @Main function main() { return }\n",
    );
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_fieldless_enum_is_eligible() {
    let items = diagnostics(
        "@Derive(Copy)\nenum Tone {\n    Red\n    Green\n}\n\
         @Main function main() { return }\n",
    );
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_string_field_is_refused_by_name() {
    let message = copy_refusal(
        "@Derive(Copy)\nstruct Label {\n    let id: Int\n    let text: String\n}\n\
         @Main function main() { return }\n",
    );
    assert!(message.contains("`Label`"), "{message}");
    assert!(message.contains("`text`"), "{message}");
    assert!(message.contains("`String`"), "{message}");
}

#[test]
fn an_array_field_is_refused() {
    let message = copy_refusal(
        "@Derive(Copy)\nstruct Bag {\n    let items: [Int]\n}\n\
         @Main function main() { return }\n",
    );
    assert!(message.contains("`items`"), "{message}");
}

#[test]
fn a_transitively_non_copyable_field_names_the_member_that_owns_it() {
    let message = copy_refusal(
        "struct Label {\n    let text: String\n}\n\
         @Derive(Copy)\nstruct Holder {\n    let inner: Label\n}\n\
         @Main function main() { return }\n",
    );
    assert!(message.contains("`Holder`"), "{message}");
    assert!(message.contains("`Label`"), "{message}");
    assert!(message.contains("`text`"), "{message}");
}

#[test]
fn a_non_copyable_enum_payload_is_refused() {
    let message = copy_refusal(
        "@Derive(Copy)\nenum Tagged {\n    Bare\n    Named: String\n}\n\
         @Main function main() { return }\n",
    );
    assert!(message.contains("`Named`"), "{message}");
}

/// The derive grants nothing: an unannotated type with the same shape is
/// classified exactly as it was, so `Copy` cannot be used to smuggle a move
/// past the ownership rules.
#[test]
fn the_derive_changes_nothing_about_an_eligible_type() {
    let with = diagnostics(
        "@Derive(Copy)\nstruct Point {\n    var x: Int\n}\n\
         function take(p: Point) -> Int { return p.x }\n\
         @Main function main() {\n    let p = Point { x: 1 }\n    print(take(p))\n    print(p.x)\n    return\n}\n",
    );
    let without = diagnostics(
        "struct Point {\n    var x: Int\n}\n\
         function take(p: Point) -> Int { return p.x }\n\
         @Main function main() {\n    let p = Point { x: 1 }\n    print(take(p))\n    print(p.x)\n    return\n}\n",
    );
    fn codes(items: &[Diagnostic]) -> Vec<&str> {
        items.iter().filter_map(Diagnostic::code_text).collect()
    }
    assert_eq!(codes(&with), codes(&without), "{with:?} vs {without:?}");
}
