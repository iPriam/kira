//! Parity for `attempt`/`try`/`handle`: VM == LLVM == hybrid.
//!
//! The construct is a desugar onto the `if`/`else` chain and the payload read
//! that `match` already proved, so the control flow is not what is under test.
//! Two things are.
//!
//! The first is the **nested enum payload**. A `Result`-shaped value carries its
//! failure enum inside its `Error` variant, which is the first payload that is
//! itself a heap handle rather than a scalar or a string. Each backend reclaims
//! it differently — the VM recurses through `Heap::free_enum`, the native box
//! recurses on `EnumPayloadKind::ENUM`, wasm never frees at all — so a backend
//! that forgot the recursion leaks (and trips the leak check) while one that
//! freed it twice crashes.
//!
//! The second is the **shape of the desugar**: the statements after a `try` are
//! nested into its success branch, which is what makes a handler-and-`try`
//! function a definite return with no trailing `return`. A backend cannot
//! disagree about that, but the analyzer can get it wrong, and it shows up here
//! as a compile failure rather than a divergence.

use crate::assert_parity;

/// The corpus `emxProcess` shape: payload-less failure variants, arms that all
/// return, and no trailing `return` after the `attempt`.
#[test]
fn payloadless_failure_variants_route_to_their_arms() {
    let output = assert_parity(
        r#"
enum ClampError { TooSmall TooBig }

enum ClampOutcome {
    Ok: Int
    Error: ClampError
}

function clamp(n: Int) -> ClampOutcome {
    if n < 0 {
        return .Error(.TooSmall)
    }
    if n > 100 {
        return .Error(.TooBig)
    }
    return .Ok(n)
}

function process(n: Int) -> Int {
    attempt {
        let v = try clamp(n)
        return v * 2
    } handle {
        TooSmall { return 0 - 1 }
        TooBig { return 0 - 2 }
    }
}

@Main
function main() {
    print(process(50))
    print(process(0 - 5))
    print(process(200))
    return
}
"#,
    );
    assert_eq!(output, "100\n-1\n-2\n");
}

/// A handler binds the failure's payload and reads it — the nested handle has to
/// survive being projected out of two boxes.
#[test]
fn a_handler_binds_and_reads_the_failure_payload() {
    let output = assert_parity(
        r#"
enum RenderFailure {
    MissingNode: String = "missing"
    InvalidState: String = "invalid"
}

enum RenderOutcome {
    Ok: Int
    Error: RenderFailure
}

function renderOk() -> RenderOutcome {
    return .Ok(42)
}

function renderFail(useMissing: Bool) -> RenderOutcome {
    if useMissing {
        return .Error(.MissingNode("boom"))
    }
    return .Error(.InvalidState("halt"))
}

function value() -> Int {
    attempt {
        let v = try renderOk()
        return v
    } handle {
        MissingNode(reason) { return 0 - 1 }
        InvalidState(reason) { return 0 - 2 }
    }
}

function reasonOf(useMissing: Bool) -> String {
    attempt {
        let v = try renderFail(useMissing)
        return "ok"
    } handle {
        MissingNode(reason) { return reason }
        InvalidState(reason) { return reason }
    }
}

@Main
function main() {
    print(value())
    print(reasonOf(true))
    print(reasonOf(false))
    return
}
"#,
    );
    assert_eq!(output, "42\nboom\nhalt\n");
}

/// Two `try`s in one body. The second nests inside the first's success branch,
/// and both route to the same handlers — which the desugar reaches by emitting
/// the arms twice.
#[test]
fn two_tries_in_one_body_both_route_to_the_handlers() {
    let output = assert_parity(
        r#"
enum StepError { First Second }

enum StepOutcome {
    Ok: Int
    Error: StepError
}

function step(n: Int, failAt: Int) -> StepOutcome {
    if n == failAt {
        if n == 1 {
            return .Error(.First)
        }
        return .Error(.Second)
    }
    return .Ok(n * 10)
}

function run(failAt: Int) -> Int {
    attempt {
        let a = try step(1, failAt)
        let b = try step(2, failAt)
        return a + b
    } handle {
        First { return 0 - 1 }
        Second { return 0 - 2 }
    }
}

@Main
function main() {
    print(run(0))
    print(run(1))
    print(run(2))
    return
}
"#,
    );
    assert_eq!(output, "30\n-1\n-2\n");
}

/// Statements before a `try` run before it, and statements after it run only on
/// success — the nesting the desugar builds, observed.
#[test]
fn statements_around_a_try_run_in_written_order() {
    let output = assert_parity(
        r#"
enum LoadError { Missing }

enum LoadOutcome {
    Ok: Int
    Error: LoadError
}

function load(ok: Bool) -> LoadOutcome {
    if ok {
        return .Ok(7)
    }
    return .Error(.Missing)
}

function run(ok: Bool) -> Int {
    attempt {
        print("before")
        let v = try load(ok)
        print("after")
        return v
    } handle {
        Missing { return 0 - 1 }
    }
}

@Main
function main() {
    print(run(true))
    print(run(false))
    return
}
"#,
    );
    assert_eq!(output, "before\nafter\n7\nbefore\n-1\n");
}

/// A `Result`-shaped enum is recognized structurally, not by name: the reference
/// `try`s a locally declared `Outcome`, so requiring a particular declared type
/// would reject a program it accepts.
#[test]
fn any_ok_error_enum_is_result_shaped() {
    let output = assert_parity(
        r#"
enum Failure { Bad }

enum Whatever {
    Ok: String
    Error: Failure
}

function get(ok: Bool) -> Whatever {
    if ok {
        return .Ok("fine")
    }
    return .Error(.Bad)
}

function run(ok: Bool) -> String {
    attempt {
        let v = try get(ok)
        return v
    } handle {
        Bad { return "bad" }
    }
}

@Main
function main() {
    print(run(true))
    print(run(false))
    return
}
"#,
    );
    assert_eq!(output, "fine\nbad\n");
}

/// A nested enum payload built, bound, and matched on outside any `attempt` —
/// the representation change on its own, independent of the construct that
/// needed it.
#[test]
fn a_nested_enum_payload_round_trips_through_a_match() {
    let output = assert_parity(
        r#"
enum Inner {
    Text: String = "inner"
    Number: Int
}

enum Outer {
    Wrapped: Inner
    Empty
}

function describe(o: borrow Outer) -> String {
    match o {
        Wrapped(inner) -> {
            match inner {
                Text(t) -> { return t }
                Number(n) -> { return "number" }
            }
        }
        Empty -> { return "empty" }
    }
}

@Main
function main() {
    let a: Outer = .Wrapped(.Text("deep"))
    let b: Outer = .Wrapped(.Number(5))
    let c: Outer = .Empty
    print(describe(a))
    print(describe(b))
    print(describe(c))
    return
}
"#,
    );
    assert_eq!(output, "deep\nnumber\nempty\n");
}
