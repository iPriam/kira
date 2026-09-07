use super::*;

// ----- parameter bounds ----------------------------------------------------
//
// A bound discharges at instantiation: substituting a type that does not keep
// the trait's promise is refused there, naming the trait and the type. The
// answer reads the conformance table and the derived marker facts — the same
// tables every other trait question reads — so nothing about bounds exists
// below semantics.

/// A scored type, an unscored one, and a template demanding the promise.
const BOUNDED: &str = "trait Scored { function score(borrow self) -> Int }\n\
                       struct Mark: Scored {\n\
                           let n: Int\n\
                           function score(borrow self) -> Int { return n }\n\
                       }\n\
                       struct Plain { let n: Int }\n\
                       enum Boxed<Value: Scored> { Held(Value) Empty }\n";

#[test]
fn a_conforming_argument_satisfies_the_bound() {
    assert!(
        codes(&format!(
            "{BOUNDED}\
             @Main function main() {{\n\
                 let held: Boxed<Mark> = .Held(Mark(n: 3))\n\
                 let bare: Boxed<Mark> = .Empty\n\
                 match held {{ Held -> {{ print(1) }} Empty -> {{ print(0) }} }}\n\
                 match bare {{ Held -> {{ print(1) }} Empty -> {{ print(0) }} }}\n\
                 return\n\
             }}"
        ))
        .is_empty()
    );
}

#[test]
fn an_argument_lacking_the_conformance_is_refused_naming_both_parties() {
    // A struct-shaped argument with no recorded conformance names its fix.
    assert!(
        codes(&format!(
            "{BOUNDED}\
             @Main function main() {{ let no: Boxed<Plain> = .Empty print(1) return }}"
        ))
        .iter()
        .any(|code| code == "KSEM315")
    );
    // And so does a builtin, which can never conform at all.
    assert!(
        codes(&format!(
            "{BOUNDED}\
             @Main function main() {{ let no: Boxed<Int> = .Empty print(1) return }}"
        ))
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn a_bound_fires_for_an_instantiation_a_declaration_mints() {
    // A field's type resolves long before the conformance table exists, so the
    // check has to wait for it — which is what the queue is for.
    assert!(
        codes(&format!(
            "{BOUNDED}\
             struct Slot {{ var held: Boxed<Plain> }}\n\
             @Main function main() {{ print(1) return }}"
        ))
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn several_bounds_on_one_parameter_all_hold() {
    assert!(
        codes(
            "trait Scored { function score(borrow self) -> Int }\n\
             trait Tagged {}\n\
             struct Mark: Scored, Tagged {\n\
                 let n: Int\n\
                 function score(borrow self) -> Int { return n }\n\
             }\n\
             struct PlainOnly: Tagged { let n: Int }\n\
             enum Both<Value: Scored + Tagged> { One(Value) }\n\
             @Main function main() {\n\
                 let ok: Both<Mark> = .One(Mark(n: 1))\n\
                 print(1)\n\
                 return\n\
             }"
        )
        .is_empty()
    );
    // One bound kept and one broken is still broken: every trait named on the
    // parameter must be kept by the argument.
    assert!(
        codes(
            "trait Scored { function score(borrow self) -> Int }\n\
             trait Tagged {}\n\
             struct Mark: Scored, Tagged {\n\
                 let n: Int\n\
                 function score(borrow self) -> Int { return n }\n\
             }\n\
             struct PlainOnly: Tagged { let n: Int }\n\
             enum Both<Value: Scored + Tagged> { One(Value) }\n\
             @Main function main() {\n\
                 let no: Both<PlainOnly> = .One(PlainOnly(n: 1))\n\
                 print(1)\n\
                 return\n\
             }"
        )
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn a_bound_holds_its_supertraits_obligation_too() {
    // Slice 1a's rule, asked of an argument: keeping `Ordered` means keeping
    // `Equated`, so an argument whose own claim left that unmet fails the
    // bound as well.
    assert!(
        codes(
            "trait Equated { function equals(borrow self, other: Int) -> Bool }\n\
             trait Ordered: Equated { function less(borrow self, other: Int) -> Bool }\n\
             struct Half: Ordered {\n\
                 let n: Int\n\
                 function less(borrow self, other: Int) -> Bool { return n < other }\n\
             }\n\
             enum Pair<T: Ordered> { Of(T) }\n\
             @Main function main() { let p: Pair<Half> = .Of(Half(n: 1)) print(1) return }"
        )
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn a_supertrait_met_by_a_retroactive_claim_discharges_the_bound() {
    // The obligation may be discharged after the fact, exactly as slice 1a
    // allows for a written conformance.
    assert!(
        codes(
            "trait Equated { function equals(borrow self, other: Int) -> Bool }\n\
             trait Ordered: Equated { function less(borrow self, other: Int) -> Bool }\n\
             struct Late: Ordered {\n\
                 let n: Int\n\
                 function less(borrow self, other: Int) -> Bool { return n < other }\n\
             }\n\
             extend Late: Equated {\n\
                 function equals(borrow self, other: Int) -> Bool { return n * 2 == other }\n\
             }\n\
             enum Pair<T: Ordered> { Of(T) }\n\
             @Main function main() { let p: Pair<Late> = .Of(Late(n: 2)) print(1) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_copyable_bound_is_answered_by_the_derived_fact() {
    // `Int` copies; a struct owning heap storage does not. Both answers come
    // from the same walk `@Derive(Copy)` uses.
    assert!(
        codes(
            "enum Copied<Value: Copyable> { One(Value) }\n\
             @Main function main() { let ok: Copied<Int> = .One(4) print(ok == ok) return }"
        )
        .is_empty()
    );
    assert!(
        codes(
            "enum Copied<Value: Copyable> { One(Value) }\n\
             struct Heapful { let text: String }\n\
             @Main function main() { let no: Copied<Heapful> = .One(Heapful(text: \"x\")) print(1) return }"
        )
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn a_send_bound_is_answered_by_the_same_walk_the_task_boundary_uses() {
    // `Int` moves; the erased top type carries every marker the walk answers,
    // exactly as it does at a task boundary, because it is the same question.
    assert!(
        codes(
            "enum Moved<Value: Send> { One(Value) }\n\
             @Main function main() { let ok: Moved<Int> = .One(4) print(ok == ok) return }"
        )
        .is_empty()
    );
    assert!(
        codes(
            "enum Moved<Value: Send> { One(Value) }\n\
             @Main function main() { let ok: Moved<Any> = .One(4) print(ok == ok) return }"
        )
        .is_empty()
    );
}

#[test]
fn a_drop_bound_is_answered_by_whether_a_body_runs() {
    assert!(
        codes(
            "enum Gone<Value: Drop> { Held(Value) }\n\
             @Main function main() { let no: Gone<Int> = .Held(1) print(1) return }"
        )
        .iter()
        .any(|code| code == "KSEM315")
    );
}

#[test]
fn a_bound_naming_no_trait_is_refused_at_the_declaration() {
    assert!(
        codes(
            "struct Plain { let n: Int }\n\
             enum Mystery<Value: Plain> { Thing(Value) }\n\
             @Main function main() { print(1) return }"
        )
        .iter()
        .any(|code| code == "KSEM289")
    );
}

#[test]
fn an_unbounded_parameter_takes_every_argument_as_before() {
    // No bound, no new restriction and no new capability: the same template
    // without the colon admits what it always admitted.
    assert!(
        codes(
            "trait Scored { function score(borrow self) -> Int }\n\
             struct Mark: Scored {\n\
                 let n: Int\n\
                 function score(borrow self) -> Int { return n }\n\
             }\n\
             struct Plain { let n: Int }\n\
             enum Boxed<Value> { Held(Value) Empty }\n\
             @Main function main() {\n\
                 let a: Boxed<Int> = .Empty\n\
                 let b: Boxed<Plain> = .Empty\n\
                 let c: Boxed<Mark> = .Empty\n\
                 print(1)\n\
                 return\n\
             }"
        )
        .is_empty()
    );
}

#[test]
fn a_widened_spelling_of_a_bounded_template_is_still_an_instantiation() {
    // Widening needs the target row to exist, and writing it goes through the
    // same discharge as any other instantiation — `Any` satisfies none of the
    // declared traits, so a bounded template does not widen by spelling.
    assert!(
        codes(&format!(
            "{BOUNDED}\
             @Main function main() {{\
                 let held: Boxed<Mark> = .Held(Mark(n: 3))\
                 let wide: Boxed<Any> = held\
                 print(1)\
                 return\
             }}"
        ))
        .iter()
        .any(|code| code == "KSEM315")
    );
}
