//! `Send` and `Sync`: what the compiler derives, what a claim asserts, and what
//! a task spawn demands of the signature it defers.

use super::*;

const MAIN: &str = "@Main function main() { return }\n";

/// A program whose declarations and `@Main` are the whole file.
fn program(body: &str) -> String {
    format!("{body}{MAIN}")
}

#[test]
fn a_send_claim_on_a_scalar_struct_is_accepted() {
    let items = diagnostics(&program(
        "struct Point: Send, Sync {\n    let x: Int\n    let y: Float\n}\n",
    ));
    assert!(items.is_empty(), "{items:?}");
}

/// A heap value has one owner, and a move leaves the receiving thread the only
/// holder — so owning storage does not stop a value crossing.
#[test]
fn a_heap_owning_struct_is_send_and_sync() {
    let items = diagnostics(&program(
        "struct Label: Send, Sync {\n    let text: String\n    let parts: [String]\n}\n",
    ));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_send_claim_refuted_by_a_function_type_member_names_it() {
    let items = diagnostics(&program(
        "struct Holder: Send {\n    let cb: () -> Void\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM311"))
        .unwrap_or_else(|| panic!("expected a KSEM311, got {items:?}"));
    assert!(refusal.message.contains("`cb`"), "{refusal:?}");
    assert!(refusal.message.contains("() -> Void"), "{refusal:?}");
    assert!(
        refusal.message.contains("may be moved to another thread"),
        "{refusal:?}"
    );
}

#[test]
fn a_sync_claim_is_refuted_by_the_same_member_with_its_own_promise() {
    let items = diagnostics(&program(
        "struct Holder: Sync {\n    let cb: () -> Void\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM311"))
        .unwrap_or_else(|| panic!("expected a KSEM311, got {items:?}"));
    assert!(
        refusal
            .message
            .contains("may be borrowed from more than one thread at once"),
        "{refusal:?}"
    );
}

/// The reason names the type that owns the offending member, not the one the
/// claim was written on, because that is where the fix goes.
#[test]
fn a_refusal_reached_through_a_field_names_the_type_that_owns_it() {
    let items = diagnostics(&program(
        "struct Inner {\n    let cb: () -> Void\n}\nstruct Outer: Send {\n    let inner: Inner\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM311"))
        .unwrap_or_else(|| panic!("expected a KSEM311, got {items:?}"));
    assert!(refusal.message.contains("`Inner`'s member"), "{refusal:?}");
}

#[test]
fn an_array_of_a_refuting_element_refutes_its_holder() {
    let items = diagnostics(&program(
        "struct Bank: Send {\n    let handlers: [() -> Void]\n}\n",
    ));
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM311".to_owned()), "{items:?}");
}

#[test]
fn an_enum_payload_that_refutes_a_marker_refutes_the_enum() {
    let items = diagnostics(&program(
        "enum Step {\n    case Done\n    case Then(() -> Void)\n}\n\
         struct Plan: Send {\n    let next: Step\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM311"))
        .unwrap_or_else(|| panic!("expected a KSEM311, got {items:?}"));
    assert!(refusal.message.contains("`Then`"), "{refusal:?}");
}

#[test]
fn a_marker_may_not_be_declared_in_source() {
    for name in ["Send", "Sync"] {
        let items = diagnostics(&program(&format!("trait {name} {{}}\n")));
        let codes: Vec<String> = items
            .iter()
            .filter_map(Diagnostic::code_text)
            .map(str::to_owned)
            .collect();
        assert_eq!(codes, vec!["KSEM288".to_owned()], "{name}");
    }
}

/// A declared trait may require a marker, and the obligation is discharged by
/// the derived fact rather than by writing the marker out a second time.
#[test]
fn a_trait_requiring_send_is_satisfied_by_the_derived_fact() {
    let items = diagnostics(&program(
        "trait Portable: Send {}\nstruct Point: Portable {\n    let x: Int\n}\n",
    ));
    assert!(items.is_empty(), "{items:?}");
}

#[test]
fn a_trait_requiring_send_refuses_a_type_that_does_not_carry_it() {
    let items = diagnostics(&program(
        "trait Portable: Send {}\nstruct Holder: Portable {\n    let cb: () -> Void\n}\n",
    ));
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM310"))
        .unwrap_or_else(|| panic!("expected a KSEM310, got {items:?}"));
    assert!(refusal.message.contains("`Send`"), "{refusal:?}");
    assert!(refusal.message.contains("`cb`"), "{refusal:?}");
}

/// A spawn is the one boundary a value crosses without its spawner, so what the
/// deferred call takes and returns must be movable.
#[test]
fn a_task_body_taking_a_value_that_cannot_move_is_refused() {
    let items = diagnostics(
        "async function tally(cb: () -> Void) -> Int { cb() return 1 }\n\
         @Main function main() {\n    var total = 0\n\
         \n    let bump: () -> Void = { in total = total + 1 }\n\
         \n    let handle = Task { tally(bump) }\n    print(handle.await)\n    return\n}\n",
    );
    let refusal = items
        .iter()
        .find(|item| item.has_code("KSEM312"))
        .unwrap_or_else(|| panic!("expected a KSEM312, got {items:?}"));
    assert!(refusal.message.contains("`tally`"), "{refusal:?}");
    assert!(refusal.message.contains("cannot cross"), "{refusal:?}");
}

/// The representation rule is narrower than the `Send` rule today: a `String`
/// moves between threads, and a task slot still holds one machine word, so it
/// is `KSEM159` that refuses this and not `KSEM312`.
#[test]
fn a_task_body_taking_a_sendable_non_scalar_is_refused_by_the_slot_rule() {
    let items = diagnostics(
        "async function count(text: String) -> Int { return text.count }\n\
         @Main function main() {\n    let handle = Task { count(\"ab\") }\n\
         \n    print(handle.await)\n    return\n}\n",
    );
    let codes: Vec<String> = items
        .iter()
        .filter_map(Diagnostic::code_text)
        .map(str::to_owned)
        .collect();
    assert!(codes.contains(&"KSEM159".to_owned()), "{items:?}");
    assert!(!codes.contains(&"KSEM312".to_owned()), "{items:?}");
}

#[test]
fn a_scalar_task_body_passes_both_rules() {
    let items = diagnostics(
        "async function twice(n: Int) -> Int { return n * 2 }\n\
         @Main function main() {\n    let handle = Task { twice(21) }\n\
         \n    print(handle.await)\n    return\n}\n",
    );
    assert!(items.is_empty(), "{items:?}");
}
