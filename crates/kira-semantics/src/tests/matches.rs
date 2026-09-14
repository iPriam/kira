//! Semantic-analysis tests for `match`: variant resolution, payload bindings,
//! and the two checks `match` has —
//! exhaustive coverage and duplicate arms.

use super::{codes, diagnostics};

/// The corpus shape: arrow arms, every one returning, and no trailing
/// `return`. It type-checks only because an exhaustive match whose arms all
/// return is itself a definite return.
#[test]
fn an_exhaustive_match_of_returning_arms_is_a_definite_return() {
    assert!(
        codes(
            "enum Shade { Light Mid Dark }\n\
             function rank(s: borrow Shade) -> Int { match s { Light -> return 1 \
             Mid -> return 2 Dark -> return 3 } }\n\
             @Main function main() { let d: Shade = .Dark print(rank(d)) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_payload_binding_is_typed_by_its_variant() {
    assert!(
        codes(
            "enum Note { Tag(String) Rank(Int) Blank }\n\
             function textOf(n: borrow Note) -> String { match n { Tag(t) -> return t \
             Rank(r) -> return \"rank\" Blank -> return \"blank\" } }\n\
             @Main function main() { let n: Note = .Blank print(textOf(n)) return }"
        )
        .is_empty()
    );
}

/// A binding takes the payload's type, not the enum's — using it as the wrong
/// type is reported against the use.
#[test]
fn a_payload_binding_used_at_the_wrong_type_is_reported() {
    assert_eq!(
        codes(
            "enum Note { Rank(Int) Blank }\n\
             function f(n: borrow Note) -> String { match n { Rank(r) -> return r \
             Blank -> return \"b\" } }\n\
             @Main function main() { let n: Note = .Blank print(f(n)) return }"
        ),
        vec!["KSEM032"],
        "the binding is an `Int`, so returning it from a `String` function is the error"
    );
}

/// The binding belongs to its own arm: two arms may bind the same name, and
/// neither name outlives the match.
#[test]
fn a_binding_does_not_escape_its_arm() {
    assert_eq!(
        codes(
            "enum Pair { Left(Int) Right(Int) }\n\
             @Main function main() { let p: Pair = .Left(1)\n\
             match p { Left(v) -> { print(v) } Right(v) -> { print(v) } }\n\
             print(v) return }"
        ),
        vec!["KSEM060"],
        "each arm binds `v` for itself; after the match there is no `v`"
    );
}

#[test]
fn a_match_missing_a_variant_is_reported() {
    assert_eq!(
        codes(
            "enum Shade { Light Mid Dark }\n\
             @Main function main() { let s: Shade = .Mid\n\
             match s { Light -> { print(1) } Mid -> { print(2) } } return }"
        ),
        vec!["KSEM129"]
    );
}

/// Every missing variant is named, so one diagnostic covers the whole gap.
#[test]
fn a_coverage_report_names_every_missing_variant() {
    let text = "enum Shade { Light Mid Dark }\n\
                @Main function main() { let s: Shade = .Mid\n\
                match s { Light -> { print(1) } } return }";
    let message = diagnostics(text)
        .into_iter()
        .find(|diagnostic| diagnostic.has_code("KSEM129"))
        .expect("a coverage diagnostic")
        .message;
    assert!(message.contains("Mid"), "{message}");
    assert!(message.contains("Dark"), "{message}");
}

#[test]
fn a_variant_matched_twice_is_reported() {
    assert_eq!(
        codes(
            "enum Shade { Light Mid Dark }\n\
             @Main function main() { let s: Shade = .Mid\n\
             match s { Light -> { print(1) } Light -> { print(9) } Mid -> { print(2) } \
             Dark -> { print(3) } } return }"
        ),
        vec!["KSEM127"],
        "the second mention is the duplicate, and coverage stays quiet"
    );
}

#[test]
fn an_unknown_variant_is_reported_once() {
    assert_eq!(
        codes(
            "enum Shade { Light Mid Dark }\n\
             @Main function main() { let s: Shade = .Mid\n\
             match s { Light -> { print(1) } Purple -> { print(2) } Mid -> { print(2) } \
             Dark -> { print(3) } } return }"
        ),
        vec!["KSEM126"],
        "a misspelled variant is one mistake, not also a coverage gap"
    );
}

#[test]
fn a_subject_that_is_neither_enum_nor_scalar_is_refused() {
    assert_eq!(
        codes(
            "struct P { var x: Int }\n\
             @Main function main() { let p = P { x: 1 } match p { Light -> { print(1) } } return }"
        ),
        vec!["KSEM125"]
    );
}

/// An `Int`, `Bool`, or `String` subject matches on literals, ranges, and an
/// `else`. A scalar match with no `else` is refused unless a `Bool` covers both
/// sides.
#[test]
fn a_scalar_match_covers_literals_ranges_and_else() {
    assert!(
        diagnostics(
            "@Main function main() { let n = 5\n\
             match n { 0 -> { print(1) } 1, 2, 3 -> { print(2) } 4..10 -> { print(3) } else -> { print(4) } }\n\
             return }"
        )
        .is_empty()
    );
    assert!(
        diagnostics(
            "@Main function main() { let b = true\n\
             match b { true -> { print(1) } false -> { print(2) } } return }"
        )
        .is_empty()
    );
    // No `else` on an `Int` is not exhaustive.
    assert_eq!(
        codes("@Main function main() { let n = 1 match n { 1 -> { print(1) } } return }"),
        vec!["KSEM380"]
    );
    // A value listed twice.
    assert_eq!(
        codes(
            "@Main function main() { let n = 1\n\
             match n { 1 -> { print(1) } 1 -> { print(2) } else -> { print(3) } } return }"
        ),
        vec!["KSEM382"]
    );
    // A pattern of the wrong type against the subject.
    assert_eq!(
        codes(
            "@Main function main() { let n = 1\n\
             match n { \"x\" -> { print(1) } else -> { print(2) } } return }"
        ),
        vec!["KSEM383"]
    );
    // An arm after `else` can never run.
    assert_eq!(
        codes(
            "@Main function main() { let n = 1\n\
             match n { else -> { print(1) } 2 -> { print(2) } } return }"
        ),
        vec!["KSEM381"]
    );
}

/// An enum arm alternates variants with commas, and `else` covers the rest.
#[test]
fn an_enum_match_alternates_and_takes_an_else() {
    assert!(
        diagnostics(
            "enum Hue { Red Green Blue Cyan }\n\
             @Main function main() { let h: Hue = .Red\n\
             match h { Red, Green -> { print(1) } else -> { print(2) } } return }"
        )
        .is_empty()
    );
    // A binding is not allowed on an alternation.
    assert_eq!(
        codes(
            "enum Tok { A(Int) B(Int) }\n\
             @Main function main() { let t: Tok = .A(1)\n\
             match t { A(x), B(x) -> { print(x) } } return }"
        ),
        vec!["KSEM384"]
    );
}

/// A bad subject must not bury itself. The arms' bodies are still analyzed, so
/// their own mistakes surface — but every binding is declared first, or each
/// use of one becomes a second "undefined name" on top of the real error.
#[test]
fn a_bad_subject_does_not_cascade_through_its_bindings() {
    assert_eq!(
        codes(
            "struct P { var x: Int }\n\
             @Main function main() { let p = P { x: 1 }\n\
             match p { Tag(t) -> { print(t) } } return }"
        ),
        vec!["KSEM125"],
        "the binding is declared even when the subject failed, so `t` resolves"
    );

    // The same, reached the other way: a moved subject types as `Error`, which
    // is already reported and must stay a single diagnostic.
    assert_eq!(
        codes(
            "enum Note { Tag(String) Blank }\n\
             @Main function main() { let a: Note = .Tag(\"x\")\n\
             let b = a\n\
             match a { Tag(t) -> { print(t) } Blank -> { print(\"b\") } } return }"
        ),
        vec!["KSEM107"]
    );
}

#[test]
fn binding_a_payload_less_variant_is_refused() {
    assert_eq!(
        codes(
            "enum Shade { Light Mid }\n\
             @Main function main() { let s: Shade = .Mid\n\
             match s { Light(x) -> { print(1) } Mid -> { print(2) } } return }"
        ),
        vec!["KSEM128"]
    );
}

/// Ignoring a payload is legal: the corpus writes `Empty -> return 0` beside
/// `Filled(shape) -> …`, and nothing requires a payload be named.
#[test]
fn an_arm_may_ignore_a_payload_it_does_not_need() {
    assert!(
        codes(
            "enum Note { Tag(String) Blank }\n\
             @Main function main() { let n: Note = .Blank\n\
             match n { Tag -> { print(1) } Blank -> { print(2) } } return }"
        )
        .is_empty()
    );
}
