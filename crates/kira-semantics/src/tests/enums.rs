//! Semantic-analysis tests for enums: declaration, leading-dot resolution, tag
//! equality, payloads and defaults, and the ownership rules an enum shares with
//! an array.

use super::codes;

#[test]
fn a_payload_less_enum_and_its_equality_type_check() {
    assert!(
        codes(
            "enum Color { Red Green Blue }\n\
             function rank(c: Color) -> Int { if c == .Red { return 1 } return 2 }\n\
             @Main function main() { print(rank(.Green)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_leading_dot_needs_an_enum_type_to_resolve_against() {
    // No expected type at all: a bare `.Red` in a `print` argument.
    assert_eq!(
        codes("enum Color { Red }\n@Main function main() { print(.Red) return }"),
        // The unresolved dot, then the unprintable-value error on the Error node
        // does not fire because the argument already errored — one diagnostic.
        vec!["KSEM119"]
    );
}

#[test]
fn a_leading_dot_against_a_non_enum_type_is_refused() {
    assert_eq!(
        codes("@Main function main() { let x: Int = .Red return }"),
        vec!["KSEM119"]
    );
}

#[test]
fn an_unknown_variant_is_reported() {
    assert_eq!(
        codes(
            "enum Color { Red Green }\n\
             @Main function main() { let c: Color = .Purple return }"
        ),
        vec!["KSEM120"]
    );
}

#[test]
fn a_duplicate_enum_and_a_duplicate_variant_are_reported() {
    assert_eq!(
        codes("enum C { A }\nenum C { B }\n@Main function main() { return }"),
        vec!["KSEM006"]
    );
    assert_eq!(
        codes("enum C { A A }\n@Main function main() { return }"),
        vec!["KSEM007"]
    );
}

#[test]
fn a_payload_less_variant_takes_no_argument() {
    assert_eq!(
        codes(
            "enum C { A B }\n\
             @Main function main() { let c: C = .A(1) return }"
        ),
        vec!["KSEM121"]
    );
}

#[test]
fn a_payload_variant_uses_its_default_when_none_is_written() {
    assert!(
        codes(
            "enum E { Bad: String = \"oops\" Ok }\n\
             function code(e: E) -> Int { if e == .Ok { return 0 } return 1 }\n\
             @Main function main() { print(code(.Bad)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_payload_variant_without_an_argument_or_default_is_reported() {
    assert_eq!(
        codes(
            "enum E { Wrap(Int) }\n\
             @Main function main() { let e: E = .Wrap return }"
        ),
        vec!["KSEM124"]
    );
}

#[test]
fn a_payload_of_the_wrong_type_is_reported() {
    assert_eq!(
        codes(
            "enum E { Wrap(Int) }\n\
             @Main function main() { let e: E = .Wrap(\"no\") return }"
        ),
        vec!["KSEM123"]
    );
}

/// An aggregate payload is admitted: the box carries a struct or an array
/// through the erased aggregate slot, whose generated clone/free leaves reclaim
/// whatever the elements own.
#[test]
fn an_aggregate_payload_is_admitted_at_the_declaration() {
    assert!(
        codes(
            "enum E { Wrap([Int]) Deep([[String]]) }\n\
             @Main function main() { return }"
        )
        .is_empty()
    );
}

/// A type with no payload form at all is still refused, and `KSEM118` still
/// names the set that has one.
#[test]
fn a_payload_type_with_no_value_form_is_refused_at_the_declaration() {
    assert_eq!(
        codes(
            "enum E { Wrap(Void) }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM118"]
    );
}

/// A *nested enum* payload is admitted, unlike an aggregate one: it is a handle,
/// so it fits the box's one word. `attempt`/`try`/`handle` needs it, because a
/// `Result`-shaped value carries its failure enum inside `Error`. Every layer
/// reclaims it recursively — see `.codex/work/attempt.md`.
#[test]
fn a_nested_enum_payload_is_admitted() {
    assert!(
        codes(
            "enum Inner { A }\n\
             enum E { Wrap(Inner) Empty }\n\
             @Main function main() { let e: E = .Wrap(.A) \
             match e { Wrap(i) -> { print(\"w\") } Empty -> { print(\"e\") } } return }"
        )
        .is_empty()
    );
}

#[test]
fn an_enum_moves_on_binding_like_an_array() {
    // `let alias = e` consumes `e`; touching it after is use-after-move — the
    // same rule an array follows, driven by the shared `moves_on_bind`.
    assert_eq!(
        codes(
            "enum C { A B }\n\
             @Main function main() { let e: C = .A\n let alias = e\n let again: C = e return }"
        ),
        vec!["KSEM107"]
    );
}

#[test]
fn passing_a_named_enum_to_an_owned_parameter_needs_move() {
    // An enum is not trivially copyable, so a *named* enum local reaches an
    // owned parameter only with `move` — a fresh `.Variant` needs nothing.
    assert_eq!(
        codes(
            "enum C { A }\n\
             function take(c: C) -> Int { return 0 }\n\
             @Main function main() { let e: C = .A\n print(take(e)) return }"
        ),
        vec!["KSEM108"]
    );
    assert!(
        codes(
            "enum C { A }\n\
             function take(c: C) -> Int { return 0 }\n\
             @Main function main() { let e: C = .A\n print(take(move e)) return }"
        )
        .is_empty()
    );
}

#[test]
fn an_enum_cannot_be_printed() {
    assert_eq!(
        codes(
            "enum C { A }\n\
             @Main function main() { let e: C = .A\n print(e) return }"
        ),
        vec!["KSEM081"]
    );
}
