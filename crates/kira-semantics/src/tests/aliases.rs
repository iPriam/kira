//! Semantics tests for `type Name = Target`: what an alias resolves to, and
//! the two ways a program can get one wrong.

use super::{codes, diagnostics};

#[test]
fn an_alias_resolves_everywhere_a_type_can_be_written() {
    assert!(
        diagnostics(
            "type Count = Int
             type Buffer = [Count]
             struct Packet { var payload: Buffer }
             function sum(buffer: borrow Buffer) -> Count {
                 var total = 0
                 for value in buffer { total = total + value }
                 return total
             }
             @Main function main() {
                 var payload: Buffer = []
                 payload.append(1)
                 let packet = Packet { payload: move payload }
                 print(sum(packet.payload))
                 return
             }"
        )
        .is_empty()
    );
}

/// An alias is a spelling, not a nominal type: `Count` and `Int` are the same
/// type, so a value of one goes anywhere the other does.
#[test]
fn an_alias_is_the_same_type_as_its_target() {
    assert!(
        diagnostics(
            "type Count = Int
             function double(n: Int) -> Int { return n * 2 }
             @Main function main() {
                 let n: Count = 3
                 let m: Int = n
                 print(double(n) + m)
                 return
             }"
        )
        .is_empty()
    );
    // And it is *not* compatible with a different target, for the same reason.
    assert_eq!(
        codes(
            "type Count = Int
             @Main function main() { let n: Count = \"three\" return }"
        ),
        vec!["KSEM020"]
    );
}

/// Aliases chain through each other, and declaration order is not a rule for
/// them — resolution is lazy, so a later alias is reachable from an earlier one.
#[test]
fn aliases_chain_in_either_declaration_order() {
    assert!(
        diagnostics(
            "type Matrix = [Buffer]
             type Buffer = [Count]
             type Count = Int
             @Main function main() {
                 var rows: Matrix = []
                 print(rows.count)
                 return
             }"
        )
        .is_empty()
    );
}

/// A cycle is reported rather than resolved, which is what keeps the lazy
/// resolver from recursing forever.
#[test]
fn a_cyclic_alias_is_reported_once_and_terminates() {
    assert_eq!(
        codes(
            "type A = B
             type B = A
             @Main function main() { let x: A = 1 print(x) return }"
        )
        .first()
        .copied(),
        Some("KSEM157")
    );
}

#[test]
fn a_self_referential_alias_through_an_array_terminates() {
    let reported = codes(
        "type A = [A]
         @Main function main() { var xs: A = [] print(xs.count) return }",
    );
    assert!(reported.contains(&"KSEM157"), "{reported:?}");
}

/// A name that already means something is rejected rather than shadowing: a
/// silently-ignored `type Int = Float` would type-check as `Int` and give a
/// wrong answer instead of an error.
#[test]
fn an_alias_may_not_claim_a_name_that_is_already_taken() {
    assert_eq!(
        codes("type Int = Float\n@Main function main() { return }"),
        vec!["KSEM130"]
    );
    assert_eq!(
        codes("type A = Int\ntype A = Bool\n@Main function main() { return }"),
        vec!["KSEM130"]
    );
    assert_eq!(
        codes("struct P { var x: Int }\ntype P = Int\n@Main function main() { return }"),
        vec!["KSEM130"]
    );
    assert_eq!(
        codes("enum E { A B }\ntype E = Int\n@Main function main() { return }"),
        vec!["KSEM130"]
    );
}

/// An alias whose target does not resolve reports at each use site, against
/// that site's own span — not once, wherever the alias happened to be touched
/// first.
#[test]
fn an_alias_to_an_unknown_type_reports_at_every_use() {
    assert_eq!(
        codes(
            "type Count = Nonexistent
             @Main function main() { let a: Count = 1 let b: Count = 2 print(a + b) return }"
        ),
        vec!["KSEM050", "KSEM050"]
    );
}
