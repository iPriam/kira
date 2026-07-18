//! Parity for `attempt`/`try`/`handle`: VM == wasm32 == wasm64.
//!
//! The construct desugars onto control flow the backends already agreed on, so
//! what is under test here is the **nested enum payload** a `Result`-shaped
//! value carries: the failure enum lives inside the `Error` variant's payload
//! slot, and on wasm that slot holds an *address*. That makes it exactly the
//! kind of value an address-width assumption breaks — a slot sized for a scalar
//! would truncate a wasm64 handle, and a box laid out for one memory would
//! misread on the other. A payload that is a `Float` and a payload that is a
//! nested enum cannot both be right unless the box is laid out by type.

use crate::assert_parity;

#[test]
fn payloadless_failure_variants_route_to_their_arms() {
    assert_parity(
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
}

/// The failure payload is a `String` reached through a nested enum — two
/// address-sized loads, on both memories.
#[test]
fn a_handler_reads_a_string_payload_through_the_failure_enum() {
    assert_parity(
        r#"
enum RenderFailure {
    MissingNode: String = "missing"
    InvalidState: String = "invalid"
}

enum RenderOutcome {
    Ok: Int
    Error: RenderFailure
}

function renderFail(useMissing: Bool) -> RenderOutcome {
    if useMissing {
        return .Error(.MissingNode("boom"))
    }
    return .Error(.InvalidState("halt"))
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
    print(reasonOf(true))
    print(reasonOf(false))
    return
}
"#,
    );
}

/// A nested enum payload beside a `Float` one: the box has to be laid out by
/// payload type, not by a single assumed width.
#[test]
fn a_nested_enum_payload_and_a_float_payload_coexist() {
    assert_parity(
        r#"
enum Inner {
    Text: String = "inner"
    Amount: Float
}

enum Outer {
    Wrapped: Inner
    Measure: Float
    Empty
}

function describe(o: borrow Outer) -> String {
    match o {
        Wrapped(inner) -> {
            match inner {
                Text(t) -> { return t }
                Amount(a) -> { return "amount" }
            }
        }
        Measure(m) -> { return "measure" }
        Empty -> { return "empty" }
    }
}

@Main
function main() {
    let a: Outer = .Wrapped(.Text("deep"))
    let b: Outer = .Wrapped(.Amount(1.5))
    let c: Outer = .Measure(2.5)
    let d: Outer = .Empty
    print(describe(a))
    print(describe(b))
    print(describe(c))
    print(describe(d))
    return
}
"#,
    );
}

#[test]
fn two_tries_in_one_body_both_route_to_the_handlers() {
    assert_parity(
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
}
