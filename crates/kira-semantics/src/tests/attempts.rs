//! Semantic-analysis tests for `attempt`/`try`/`handle`: the `Result` shape a
//! `try` demands, the positions it is allowed in, and the four checks the
//! reference pins on the handlers.

use super::{codes, diagnostics};

/// A `Result`-shaped enum is recognized structurally. Nothing declares a type
/// named `Result` here, and the reference's own failing tests `try` a locally
/// declared `Outcome`, so a nominal check would reject a program it accepts.
#[test]
fn a_locally_declared_ok_error_enum_is_result_shaped() {
    assert!(
        codes(
            "enum ClampError { TooSmall TooBig }\n\
             enum ClampOutcome { Ok: Int Error: ClampError }\n\
             function clamp(n: Int) -> ClampOutcome { if n < 0 { return .Error(.TooSmall) } \
             if n > 100 { return .Error(.TooBig) } return .Ok(n) }\n\
             function process(n: Int) -> Int { attempt { let v = try clamp(n) return v * 2 } \
             handle { TooSmall { return 0 - 1 } TooBig { return 0 - 2 } } }\n\
             @Main function main() { print(process(50)) return }"
        )
        .is_empty()
    );
}

/// The corpus shape: an `attempt` whose body and every handler returns is
/// itself a definite return, so the enclosing function needs no trailing
/// `return`. That works only if the rest of the body nests into the `try`'s
/// success branch.
#[test]
fn an_attempt_whose_branches_all_return_is_a_definite_return() {
    assert!(
        codes(
            "enum E { A }\n\
             enum O { Ok: Int Error: E }\n\
             function f() -> O { return .Ok(1) }\n\
             function g() -> Int { attempt { let v = try f() return v } handle { A { return 0 } } }\n\
             @Main function main() { print(g()) return }"
        )
        .is_empty()
    );
}

/// `try` outside any `attempt` is reported, not silently unwrapped.
#[test]
fn a_try_outside_an_attempt_is_reported() {
    assert_eq!(
        codes(
            "enum E { A }\n\
             enum O { Ok: Int Error: E }\n\
             function f() -> O { return .Ok(1) }\n\
             @Main function main() { let v = try f() print(v) return }"
        ),
        ["KSEM137"]
    );
}

/// A `try` nested in a larger expression is an unsupported *position*, which is
/// the other half of the reference's `try`-outside-`attempt` diagnostic. The
/// corpus writes only `let v = try f()`, so nothing pins what
/// `g(try f(), try h())` should mean.
#[test]
fn a_try_inside_a_larger_expression_is_reported() {
    let reported = codes(
        "enum E { A }\n\
         enum O { Ok: Int Error: E }\n\
         function f() -> O { return .Ok(1) }\n\
         function g() -> Int { attempt { let v = (try f()) + 1 return v } \
         handle { A { return 0 } } }\n\
         @Main function main() { print(g()) return }",
    );
    assert!(
        reported.iter().any(|code| code == "KSEM137"),
        "got {reported:?}"
    );
}

/// `try` on something that is not `Result`-shaped is reported once — and the
/// body is not also told it contains no `try`.
#[test]
fn a_try_on_a_non_result_is_reported_once() {
    assert_eq!(
        codes(
            "enum E { A }\n\
             @Main function main() { attempt { let v = try 42 print(v) } \
             handle { A { print(\"a\") } } return }"
        ),
        ["KSEM138"]
    );
}

/// An enum with no `Ok`/`Error` pair is not `Result`-shaped either.
#[test]
fn a_try_on_an_enum_without_the_ok_error_pair_is_reported() {
    assert_eq!(
        codes(
            "enum E { A }\n\
             enum Plain { One Two }\n\
             function f() -> Plain { return .One }\n\
             @Main function main() { attempt { let v = try f() print(1) } \
             handle { A { print(\"a\") } } return }"
        ),
        ["KSEM138"]
    );
}

/// Every reachable failure variant must be handled.
#[test]
fn a_missing_handler_is_reported() {
    assert_eq!(
        codes(
            "enum AppError { NotFound Denied }\n\
             enum O { Ok: Int Error: AppError }\n\
             function f() -> O { return .Ok(1) }\n\
             @Main function main() { attempt { let v = try f() print(v) } \
             handle { NotFound { print(\"nf\") } } return }"
        ),
        ["KSEM139"]
    );
}

/// A handler naming something that is not a variant of the failure enum is
/// reported — and the missing-handler check stays quiet, so one mistake is told
/// once.
#[test]
fn an_unknown_handler_is_reported() {
    assert_eq!(
        codes(
            "enum AppError { NotFound Denied }\n\
             enum O { Ok: Int Error: AppError }\n\
             function f() -> O { return .Ok(1) }\n\
             @Main function main() { attempt { let v = try f() print(v) } \
             handle { NotFound { print(\"nf\") } Denied { print(\"d\") } Bogus { print(\"b\") } } \
             return }"
        ),
        ["KSEM140"]
    );
}

/// Every `try` in one `attempt` must fail with the same enum, because they all
/// route to the same handlers.
#[test]
fn incompatible_failure_enums_across_tries_are_reported() {
    assert_eq!(
        codes(
            "enum AppError { NotFound }\n\
             enum NetError { Timeout }\n\
             enum AppOutcome { Ok: Int Error: AppError }\n\
             enum NetOutcome { Ok: Int Error: NetError }\n\
             function loadApp() -> AppOutcome { return .Ok(1) }\n\
             function loadNet() -> NetOutcome { return .Ok(2) }\n\
             @Main function main() { attempt { let a = try loadApp() let b = try loadNet() \
             print(a) } handle { NotFound { print(\"nf\") } } return }"
        ),
        ["KSEM141"]
    );
}

/// A failure handled twice is reported against the second arm.
#[test]
fn a_repeated_handler_is_reported() {
    assert_eq!(
        codes(
            "enum AppError { NotFound Denied }\n\
             enum O { Ok: Int Error: AppError }\n\
             function f() -> O { return .Ok(1) }\n\
             @Main function main() { attempt { let v = try f() print(v) } \
             handle { NotFound { print(\"a\") } Denied { print(\"b\") } NotFound { print(\"c\") } } \
             return }"
        ),
        ["KSEM142"]
    );
}

/// An `attempt` with no `try` has nothing to resolve its handlers against. The
/// reference does not pin the program, so it is refused rather than guessed at.
#[test]
fn an_attempt_without_a_try_is_refused() {
    assert_eq!(
        codes(
            "enum E { A }\n\
             @Main function main() { attempt { print(1) } handle { A { print(\"a\") } } return }"
        ),
        ["KSEM143"]
    );
}

/// A handler's payload binding is scoped to its own arm, so two arms may bind
/// the same name to different variants' payloads.
#[test]
fn handler_payload_bindings_are_scoped_to_their_arm() {
    assert!(
        codes(
            "enum F { Missing: String = \"m\" Invalid: String = \"i\" }\n\
             enum O { Ok: Int Error: F }\n\
             function f() -> O { return .Ok(1) }\n\
             function g() -> String { attempt { let v = try f() return \"ok\" } \
             handle { Missing(reason) { return reason } Invalid(reason) { return reason } } }\n\
             @Main function main() { print(g()) return }"
        )
        .is_empty()
    );
}

/// A nested enum payload is admitted now that every layer reclaims one
/// recursively — it is what a `Result`-shaped `Error` variant carries.
#[test]
fn an_enum_payload_may_be_another_enum() {
    assert!(
        codes(
            "enum Inner { Text: String = \"t\" }\n\
             enum Outer { Wrapped: Inner Empty }\n\
             @Main function main() { let o: Outer = .Wrapped(.Text(\"x\")) \
             match o { Wrapped(i) -> { print(\"w\") } Empty -> { print(\"e\") } } return }"
        )
        .is_empty()
    );
}

/// An array payload resolves, which is what makes `Result<[Int], E>` — the
/// shape a builder returns and `attempt` routes on — a writable type.
///
/// It travels as an aggregate rather than in the one-word slot: the box owns a
/// copy of the array plus the generated leaves that clone and free it.
#[test]
fn an_array_payload_resolves() {
    assert!(
        codes(
            "enum E { Held: [Int] }\n\
             @Main function main() { print(1) return }"
        )
        .is_empty()
    );
}

/// A struct payload resolves: enum *names* are declared before structs so a
/// struct field may name an enum, and enum *payloads* are resolved after every
/// struct exists so a payload may name a struct. Both directions work because
/// the enum declaration arrives in two parts.
///
/// This case previously recorded the opposite — a struct payload could not
/// resolve at all — as accepted behavior. It was a hole, not a rule.
#[test]
fn a_struct_payload_resolves_against_a_struct_declared_anywhere() {
    assert!(
        diagnostics(
            "struct P { let x: Int = 0 }\n\
             enum E { Held: P }\n\
             @Main function main() { print(1) return }"
        )
        .is_empty()
    );
    // Declaration order does not matter in either direction: the struct may
    // also come *after* the enum that carries it.
    assert!(
        diagnostics(
            "enum E { Held: P }\n\
             struct P { let x: Int = 0 }\n\
             @Main function main() { print(1) return }"
        )
        .is_empty()
    );
}
