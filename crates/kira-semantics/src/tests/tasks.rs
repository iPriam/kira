//! What the analyzer accepts and refuses in the async task spine.

use super::{codes, diagnostics};

/// A program with the async bodies every case here spawns.
fn program(body: &str) -> String {
    format!(
        "async function tskSum(a: Int, b: Int) -> Int {{ return a + b }}\n\
         async function tskNoop(n: Int) {{ let unused = n + 1 return }}\n\
         @Main function main() {{ {body} return }}"
    )
}

#[test]
fn an_async_function_is_a_task_entry_point_and_not_a_direct_call() {
    assert_eq!(codes(&program("print(tskSum(1, 2))")), vec!["KSEM354"]);
    assert!(diagnostics(&program("let t = Task { tskSum(1, 2) } print(t.await)")).is_empty());
}

/// Only an `async function` may be a task target; an ordinary function is
/// called, not scheduled.
#[test]
fn a_task_target_must_be_async() {
    let text = "function plain(a: Int) -> Int { return a }\n\
                @Main function main() { let t = Task { plain(1) } print(t.await) return }";
    assert_eq!(codes(text), vec!["KSEM352"]);
}

/// A task owns its arguments: a target that borrows a parameter is refused.
#[test]
fn a_task_target_may_not_borrow_a_parameter() {
    let text = "async function peek(items: borrow [Int]) -> Int { return items.count }\n\
                @Main function main() { let xs: [Int] = [1] let t = Task { peek(xs) } print(t.await) return }";
    assert!(
        codes(text).contains(&"KSEM353".to_owned()),
        "{:?}",
        codes(text)
    );
}

/// A task target is chosen among overloads by its arguments, as a call is.
#[test]
fn a_task_target_resolves_among_overloads() {
    let text = "async function pick(a: Int) -> Int { return a }\n\
                async function pick(a: Int, b: Int) -> Int { return a + b }\n\
                @Main function main() { let t = Task { pick(1, 2) } print(t.await) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
}

#[test]
fn async_stays_an_ordinary_identifier_everywhere_else() {
    // Contextual, not a keyword: only the token immediately before `function`
    // reads as the marker, so a local named `async` still binds.
    assert!(
        diagnostics("@Main function main() { let async = 1 print(async) return }").is_empty(),
        "{:?}",
        codes("@Main function main() { let async = 1 print(async) return }")
    );
}

#[test]
fn a_task_handle_takes_its_three_operations_and_no_others() {
    assert!(diagnostics(&program("let h = Task { tskSum(1, 2) } print(h.await)")).is_empty());
    assert!(diagnostics(&program("let h = Task { tskSum(1, 2) } h.requestCancel()")).is_empty());
    assert!(diagnostics(&program("let h = Task { tskSum(1, 2) } h.detach()")).is_empty());
}

#[test]
fn any_other_use_of_a_task_handle_is_refused() {
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, 2) } print(h.result)")),
        vec!["KSEM158"],
        "a property that is not `.await`"
    );
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, 2) } h.join()")),
        vec!["KSEM158"],
        "a method that is neither `.requestCancel()` nor `.detach()`"
    );
    // `.await` is a property, so the call spelling is refused by name rather
    // than by arity — which is what makes the fix obvious.
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, 2) } print(h.await())")),
        vec!["KSEM158"],
    );
}

#[test]
fn a_task_handle_is_not_printable_and_does_not_cross_into_any() {
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, 2) } print(h)")),
        vec!["KSEM081"],
    );
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, 2) } let a: Any = h")),
        vec!["KSEM020"],
        "{:?}",
        codes(&program("let h = Task { tskSum(1, 2) } let a: Any = h"))
    );
}

#[test]
fn a_void_body_joins_as_an_int() {
    assert!(diagnostics(&program("let h = Task { tskNoop(5) } print(h.await + 1)")).is_empty());
}

#[test]
fn a_task_body_outside_the_executable_slice_is_refused() {
    assert!(
        codes(&program("let h = Task { tskSum(1, 2) + 1 }")).contains(&"KSEM159".to_owned()),
        "an expression that is not a direct call"
    );
    assert_eq!(
        codes(
            "async function tskText() -> String { return \"x\" }\n\
             @Main function main() { let h = Task { tskText() } return }"
        ),
        vec!["KSEM159"],
        "a result that is not a scalar"
    );
    assert_eq!(
        codes(
            "async function tskFlag(on: Bool) -> Int { return 1 }\n\
             @Main function main() { let h = Task { tskFlag(true) } return }"
        ),
        vec!["KSEM159"],
        "a parameter that is not a scalar"
    );
}

#[test]
fn a_task_body_naming_no_function_reports_the_missing_function() {
    assert_eq!(codes(&program("let h = Task { nope(1) }")), vec!["KSEM061"],);
}

#[test]
fn a_task_bodys_arguments_are_checked_against_the_target() {
    assert_eq!(
        codes(&program("let h = Task { tskSum(1) }")),
        vec!["KSEM062"],
        "too few arguments"
    );
    assert_eq!(
        codes(&program("let h = Task { tskSum(1, \"x\") }")),
        vec!["KSEM063"],
        "an argument of the wrong type"
    );
}

#[test]
fn the_suspend_points_take_the_arguments_they_declare() {
    assert!(diagnostics(&program("taskYield()")).is_empty());
    assert!(diagnostics(&program("taskSleep(5)")).is_empty());
    assert_eq!(codes(&program("taskYield(1)")), vec!["KSEM062"]);
    assert_eq!(codes(&program("taskSleep()")), vec!["KSEM062"]);
    assert_eq!(codes(&program("taskSleep(\"x\")")), vec!["KSEM063"]);
}
