//! Array analysis: literals, element-type inference, the two-member surface
//! (`.append`/`.count`), index reads and writes, `for`-in, and the ownership
//! and print refusals — the same diagnostics `kirac check` reports.

use super::{codes, diagnostics};

/// The universal empty-array idiom: `[]` has no element to infer from, so
/// the annotation is the only thing that knows what it holds.
#[test]
fn an_empty_array_literal_takes_its_element_type_from_the_annotation() {
    assert!(
        diagnostics(
            "@Main function main() { var xs: [Int] = [] xs.append(1) print(xs.count) return }"
        )
        .is_empty()
    );
}

/// With no expectation there is nothing to infer from, so Kira says so
/// rather than guessing an element type.
#[test]
fn an_empty_array_literal_with_nothing_to_infer_from_is_reported() {
    assert_eq!(
        codes("@Main function main() { let xs = [] return }"),
        vec!["KSEM104"]
    );
}

#[test]
fn an_array_literal_infers_its_element_type_from_its_elements() {
    assert!(
        diagnostics("@Main function main() { let xs = [1, 2, 3] print(xs.count) return }")
            .is_empty()
    );
    assert!(
        diagnostics(r#"@Main function main() { let xs = ["a"] print(xs.count) return }"#)
            .is_empty()
    );
}

/// Element types must agree exactly. `[1, 2.0]` is the case worth pinning:
/// Kira does not widen numbers, so this is an error rather than [Float].
#[test]
fn a_mixed_array_literal_is_reported_and_numbers_do_not_widen() {
    assert_eq!(
        codes(r#"@Main function main() { let xs = [1, "a"] return }"#),
        vec!["KSEM105"]
    );
    assert_eq!(
        codes("@Main function main() { let xs = [1, 2.0] return }"),
        vec!["KSEM105"],
        "no numeric widening: an Int array cannot hold a Float"
    );
}

#[test]
fn an_array_annotation_and_its_literal_must_agree() {
    assert_eq!(
        codes("@Main function main() { var xs: [Int] = [1.5] return }"),
        vec!["KSEM105"]
    );
    assert_eq!(
        codes("@Main function main() { var n: Int = [1] return }"),
        vec!["KSEM020"],
        "a non-array annotation says nothing about elements; the binding reports"
    );
}

#[test]
fn array_types_are_interned_so_the_same_spelling_is_the_same_type() {
    // If `[Int]` were two types, this call would not type-check.
    assert!(
        diagnostics(
            "function take(xs: [Int]) -> Int { return xs.count }\n\
                 @Main function main() { var ys: [Int] = [] print(take(move ys)) return }"
        )
        .is_empty()
    );
}

#[test]
fn nested_array_types_resolve_and_index_twice() {
    assert!(
        diagnostics(
            "@Main function main() { var g: [[Int]] = [[1, 2], [3]] print(g[0][1]) return }"
        )
        .is_empty()
    );
}

// ----- the two-member surface ---------------------------------------

#[test]
fn an_array_has_exactly_append_and_count() {
    assert_eq!(
        codes("@Main function main() { var xs: [Int] = [] xs.push(1) return }"),
        vec!["KSEM101"]
    );
    assert_eq!(
        codes("@Main function main() { var xs: [Int] = [] print(xs.length) return }"),
        vec!["KSEM101"]
    );
}

/// `.count` is a property and `.append` is a method. Confusing the two is
/// common enough to be worth its own sentence rather than "no such member".
#[test]
fn count_is_a_property_and_append_is_a_method() {
    let parens =
        diagnostics("@Main function main() { var xs: [Int] = [] print(xs.count()) return }");
    assert_eq!(parens.len(), 1);
    assert!(
        parens[0].message.contains("property"),
        "{:?}",
        parens[0].message
    );

    let bare = diagnostics("@Main function main() { var xs: [Int] = [] xs.append return }");
    assert_eq!(bare.len(), 1);
    assert!(bare[0].message.contains("method"), "{:?}", bare[0].message);
}

#[test]
fn append_takes_exactly_one_argument() {
    for text in [
        "@Main function main() { var xs: [Int] = [] xs.append() return }",
        "@Main function main() { var xs: [Int] = [] xs.append(1, 2) return }",
    ] {
        assert_eq!(codes(text), vec!["KSEM103"], "{text}");
    }
}

#[test]
fn append_type_checks_its_element() {
    assert_eq!(
        codes(r#"@Main function main() { var xs: [Int] = [] xs.append("a") return }"#),
        vec!["KSEM105"]
    );
}

// ----- places -------------------------------------------------------

/// Mutation needs a `var`, exactly as a field write does. Every corpus
/// mutation site is written `var arr: [T] = []`.
#[test]
fn mutating_through_an_immutable_binding_is_reported() {
    assert_eq!(
        codes("@Main function main() { let xs = [1] xs.append(2) return }"),
        vec!["KSEM021"]
    );
    assert_eq!(
        codes("@Main function main() { let xs = [1] xs[0] = 2 return }"),
        vec!["KSEM021"]
    );
}

/// Appending to a temporary would push onto a value discarded a moment
/// later, so it is refused rather than silently losing the write.
#[test]
fn appending_to_a_temporary_is_refused() {
    let found = diagnostics(
        "function make() -> [Int] { return [1] }\n\
             @Main function main() { make().append(2) return }",
    );
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].code, Some("KSEM025"));
}

#[test]
fn an_index_write_reaches_through_a_nested_path() {
    assert!(
            diagnostics(
                "struct Inner { var x: Int }\n\
                 struct Cell { var xs: [Int] }\n\
                 @Main function main() { var g: [[Inner]] = [[Inner { x: 1 }]] g[0][0].x = 77 print(g[0][0].x) return }"
            )
            .is_empty()
        );
}

#[test]
fn an_index_must_be_an_integer_and_only_an_array_indexes() {
    assert_eq!(
        codes(r#"@Main function main() { let xs = [1] print(xs["a"]) return }"#),
        vec!["KSEM102"]
    );
    assert_eq!(
        codes("@Main function main() { let n = 5 print(n[0]) return }"),
        vec!["KSEM100"]
    );
}

/// A bad receiver reports once, not once per pass: the type probe rolls its
/// diagnostics back and the place resolution is the one that speaks.
#[test]
fn a_bad_append_receiver_reports_exactly_once() {
    assert_eq!(
        codes("@Main function main() { var g: [[Int]] = [[1]] g[nope].append(2) return }"),
        vec!["KSEM060"]
    );
}

// ----- ownership ----------------------------------------------------

/// The predicate arrays were written for: an array binding consumes its
/// source, because two names for one heap object is what it would mean.
#[test]
fn an_array_moves_on_bind_and_the_source_is_gone() {
    assert_eq!(
        codes("@Main function main() { var xs: [Int] = [] let ys = xs print(xs.count) return }"),
        vec!["KSEM107"]
    );
}

#[test]
fn passing_an_array_to_an_owned_parameter_needs_move() {
    let text = "function take(xs: [Int]) -> Int { return xs.count }\n\
                    @Main function main() { var xs: [Int] = [] print(take(xs)) return }";
    assert_eq!(codes(text), vec!["KSEM108"]);
}

/// There is no array clone. `copy xs` is refused rather than given an
/// invented deep-copy meaning.
#[test]
fn there_is_no_array_clone() {
    assert_eq!(
        codes("@Main function main() { var xs: [Int] = [] let ys = copy xs return }"),
        vec!["KSEM116"]
    );
}

// ----- for-in -------------------------------------------------------

#[test]
fn for_over_an_array_binds_the_element_type() {
    assert!(
        diagnostics("@Main function main() { let xs = [1, 2] for x in xs { print(x) } return }")
            .is_empty()
    );
    assert!(
        diagnostics(r#"@Main function main() { let xs = ["a"] for s in xs { print(s) } return }"#)
            .is_empty()
    );
}

/// The loop variable is a fresh immutable binding, so writing to it is the
/// same error writing to any `let` is.
#[test]
fn a_for_in_variable_cannot_be_assigned() {
    assert_eq!(
        codes("@Main function main() { let xs = [1] for x in xs { x = 9 } return }"),
        vec!["KSEM021"]
    );
}

/// `for i in 0 { }` is no longer a parse error — `..` cannot be mandatory
/// once `for x in xs` exists — so analysis is what reports it.
#[test]
fn iterating_a_non_array_is_reported_by_analysis() {
    assert_eq!(
        codes("@Main function main() { for i in 0 { } return }"),
        vec!["KSEM106"]
    );
}

/// The loop only reads its array, so the array survives the loop. The
/// hidden binding the desugar introduces must not consume it.
#[test]
fn for_in_does_not_consume_its_array() {
    assert!(
            diagnostics(
                "@Main function main() { let xs = [1, 2] for x in xs { print(x) } print(xs.count) return }"
            )
            .is_empty()
        );
}

/// The hidden array, limit, and cursor the desugar introduces are bound to
/// no name, so a body may declare anything it likes.
#[test]
fn a_for_in_body_may_declare_any_name_it_likes() {
    assert!(
            diagnostics(
                "@Main function main() { let xs = [1] for x in xs { let cursor = 1 let limit = 2 let array = 3 print(cursor + limit + array) } return }"
            )
            .is_empty()
        );
}

// ----- print --------------------------------------------------------

/// Refused for the same reason a struct is: no corpus call site pins a
/// separator or a bracket, so any rendering here would be invented.
#[test]
fn printing_an_array_is_refused() {
    assert_eq!(
        codes("@Main function main() { let xs = [1] print(xs) return }"),
        vec!["KSEM081"]
    );
}

/// Deciding an `append` belongs to the array surface analyzes the receiver to
/// learn its type, which runs the receiver's ownership effects. Those are a
/// probe and are rolled back: a `move` in the receiver must not leave the
/// binding phantom-moved, or a later use reports a use-after-move that never
/// happened. The append's own error is still reported, exactly once.
#[test]
fn a_move_in_an_append_receiver_does_not_phantom_move_the_binding() {
    let reported = codes(
        "struct Bag { var xs: [Int] } \
         @Main function main() { var b = Bag { xs = [] } \
         (move b).xs.append(1) print(b.xs.count) return }",
    );
    assert!(
        !reported.contains(&"KSEM107"),
        "the rolled-back probe must not leave `b` phantom-moved: {reported:?}"
    );
    assert!(
        reported.contains(&"KSEM025"),
        "the append to a temporary is still reported: {reported:?}"
    );
}
