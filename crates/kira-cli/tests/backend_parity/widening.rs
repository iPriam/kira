//! Parity for widening a generic instantiation's type arguments to `Any`:
//! VM == LLVM == hybrid.
//!
//! This is the one place in the language where the two engines do genuinely
//! different work for one node, so it is the one place parity has to be
//! *proven* rather than argued. `Result<Int, E>.Ok(10)` is a VM `Value::Enum`
//! whose payload is already a tagged `Value::Int(10)`, so widening it costs
//! nothing and the bytecode compiler emits no instruction. On the native side
//! that same value is a box holding the ten inline, while a `Result<Any, E>`
//! holds a pointer to a second box — so the LLVM backend rebuilds.
//!
//! # Why every case here reads the payload back
//!
//! An `Any` cannot be printed or unwrapped: the language has no recovery form.
//! So a case that only widened a value and printed a tag would pass with the
//! rebuild deleted. Every case below therefore *copies* the widened payload and
//! lets it drop — which on the native side runs `kira_rt_enum_clone` and
//! `kira_rt_enum_free` on the payload word. Skipping the rebuild leaves the
//! number ten in that word, and cloning ten as a pointer is a segfault rather
//! than a wrong answer, which is exactly what makes these cases bite.
//!
//! Each loops enough times that a leaked or double-freed box shows up as a
//! crash rather than as an unnoticed imbalance.

use crate::assert_parity;

/// The oracle's `Result` verbatim, plus a failure enum and the narrow producer
/// every case widens from.
const RESULT: &str = r#"
enum AppError { NotFound Denied }
enum Result<Value, Failure> { Ok(Value) Error(Failure) }
"#;

/// Copies an erased value and lets both copies drop.
///
/// The whole point: nothing may *read* an `Any`, so holding one and dropping it
/// is the strongest thing a Kira program can say about it — and it is enough,
/// because it is the operation a mis-formed payload comes apart on.
const HOLD: &str = r#"
function hold(value: Any) -> Int {
    let copy = value
    let both: [Any] = [copy, value]
    return both.count
}
"#;

#[test]
fn an_int_payload_widens_and_stays_sound_on_every_backend() {
    let output = assert_parity(&format!(
        r#"{RESULT}{HOLD}
function narrow(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    if n > 100 {{ return .Error(.Denied) }}
    return .Ok(n * 2)
}}

function wide(n: Int) -> Result<Any, AppError> {{
    let inner = narrow(n)
    return inner
}}

function classify(n: Int) -> Int {{
    let outcome = wide(n)
    match outcome {{
        Ok(value) -> {{ return hold(move value) }}
        Error(why) -> {{
            if why == .NotFound {{ return 0 - 1 }}
            return 0 - 2
        }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 200 {{
        total = total + classify(i)
        i = i + 1
    }}
    print(total)
    print(classify(0 - 5))
    print(classify(500))
    return
}}
"#
    ));
    // `hold` answers 2: the 101 values in range give 202, the 99 above 100
    // give -198.
    assert_eq!(output, "4\n-1\n-2\n");
}

#[test]
fn every_payload_kind_widens_the_same_way_on_every_backend() {
    // A `String` payload is an owned handle, a `Bool` is one inline bit, and a
    // struct payload travels through the runtime's erased aggregate box with
    // generated clone/free leaves. Three different encodings, one rule.
    let output = assert_parity(&format!(
        r#"{RESULT}{HOLD}
struct Point {{ let label: String let x: Int }}

function texts(n: Int) -> Result<String, AppError> {{
    if n < 0 {{ return .Error(.Denied) }}
    return .Ok("a string long enough that it is really allocated")
}}
function flags(n: Int) -> Result<Bool, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    return .Ok(n > 3)
}}
function points(n: Int) -> Result<Point, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    return .Ok(Point(label: "a struct payload owning a string", x: n))
}}

function wideText(n: Int) -> Result<Any, AppError> {{ let v = texts(n)  return v }}
function wideFlag(n: Int) -> Result<Any, AppError> {{ let v = flags(n)  return v }}
function widePoint(n: Int) -> Result<Any, AppError> {{ let v = points(n)  return v }}

function tally(outcome: Result<Any, AppError>) -> Int {{
    match outcome {{
        Ok(value) -> {{ return hold(move value) }}
        Error(why) -> {{
            if why == .NotFound {{ return 0 - 1 }}
            return 0 - 2
        }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 200 {{
        total = total + tally(wideText(i))
        total = total + tally(wideFlag(i))
        total = total + tally(widePoint(i))
        i = i + 1
    }}
    print(total)
    print(tally(wideText(0 - 1)))
    print(tally(wideFlag(0 - 1)))
    print(tally(widePoint(0 - 1)))
    return
}}
"#
    ));
    assert_eq!(output, "1200\n-2\n-1\n-1\n");
}

#[test]
fn the_failure_variant_is_untouched_by_the_widening() {
    // Only the variants whose payload type changed are rebuilt. `Error` carries
    // the same enum on both sides, so it falls to the rebuild's default — and
    // its payload has to come out of the other end intact, string and all.
    let output = assert_parity(&format!(
        r#"{RESULT}{HOLD}
enum Why {{ Missing(String) Refused }}

function narrow(n: Int) -> Result<Int, Why> {{
    if n < 0 {{ return .Error(.Missing("the reason, spelled out at length")) }}
    if n > 100 {{ return .Error(.Refused) }}
    return .Ok(n)
}}
function wide(n: Int) -> Result<Any, Why> {{ let v = narrow(n)  return v }}

function reason(n: Int) -> Int {{
    let outcome = wide(n)
    match outcome {{
        Ok(value) -> {{ return hold(move value) }}
        Error(why) -> {{
            match why {{
                Missing(text) -> {{ return text.count }}
                Refused -> {{ return 0 }}
            }}
        }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 200 {{
        total = total + reason(0 - 1) + reason(i)
        i = i + 1
    }}
    print(total)
    return
}}
"#
    ));
    // 200 failures of 33 characters, 101 successes answering 2, 99 refusals.
    assert_eq!(output, "6802\n");
}

#[test]
fn a_user_template_widens_by_the_identical_path() {
    // Nothing here is named `Result` and nothing is shaped like it. If the rule
    // knew a Kira name, this would not compile.
    let output = assert_parity(&format!(
        r#"{HOLD}
enum Crate<Held> {{ Full(Held) Empty }}

function crated(n: Int) -> Crate<String> {{
    if n < 0 {{ return .Empty }}
    return .Full("a user template, not the oracle's")
}}
function wide(n: Int) -> Crate<Any> {{ let v = crated(n)  return v }}

function unpack(n: Int) -> Int {{
    let c = wide(n)
    match c {{
        Full(value) -> {{ return hold(move value) }}
        Empty -> {{ return 0 }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 200 {{
        total = total + unpack(i) + unpack(0 - 1)
        i = i + 1
    }}
    print(total)
    return
}}
"#
    ));
    assert_eq!(output, "400\n");
}

#[test]
fn a_widening_composes_with_itself_on_every_backend() {
    // The type argument being widened is itself an instantiation, so the
    // rebuild recurses into the payload it just read out.
    let output = assert_parity(&format!(
        r#"{RESULT}{HOLD}
function inner(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    return .Ok(n)
}}
function outer(n: Int) -> Result<Result<Int, AppError>, AppError> {{
    if n > 100 {{ return .Error(.Denied) }}
    return .Ok(inner(n))
}}
function wide(n: Int) -> Result<Result<Any, AppError>, AppError> {{
    let v = outer(n)
    return v
}}

function read(n: Int) -> Int {{
    let o = wide(n)
    match o {{
        Ok(nested) -> {{
            match nested {{
                Ok(value) -> {{ return hold(move value) }}
                Error(why) -> {{ return 0 - 3 }}
            }}
        }}
        Error(why) -> {{
            if why == .Denied {{ return 0 - 2 }}
            return 0 - 1
        }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 200 {{
        total = total + read(i)
        i = i + 1
    }}
    print(total)
    print(read(0 - 1))
    return
}}
"#
    ));
    // 101 in range answer 2, the 99 above 100 answer -2.
    assert_eq!(output, "4\n-3\n");
}

#[test]
fn a_template_that_never_carries_its_parameter_needs_no_rebuild() {
    // `Marker<Int>` and `Marker<Any>` are two rows with identical variants, so
    // there is nothing to rebuild and the backend emits no call. It still has to
    // agree with the VM, which is what this pins.
    let output = assert_parity(
        r#"
enum Marker<Unused> { On Off }

function marked(n: Int) -> Marker<Int> {
    if n < 0 { return .Off }
    return .On
}
function wide(n: Int) -> Marker<Any> { let v = marked(n)  return v }

function read(n: Int) -> Int {
    let m = wide(n)
    match m {
        On -> { return 1 }
        Off -> { return 0 }
    }
}

@Main
function main() {
    var total = 0
    var i = 0
    while i < 200 {
        total = total + read(i) + read(0 - 1)
        i = i + 1
    }
    print(total)
    return
}
"#,
    );
    assert_eq!(output, "200\n");
}

#[test]
fn the_widening_holds_in_every_position_that_admits_a_declared_type() {
    // The same crossing reached through a `let` annotation, an assignment, an
    // argument, an array element, a struct field, and an enum payload. Each is a
    // separate call site in the analyzer, and forgetting one is how a value
    // reaches a backend in the wrong machine form.
    let output = assert_parity(&format!(
        r#"{RESULT}{HOLD}
enum Crate<Held> {{ Full(Held) Empty }}
struct Wrapper {{ let inner: Result<Any, AppError> }}

function narrow(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.Denied) }}
    return .Ok(n)
}}
function takes(outcome: Result<Any, AppError>) -> Int {{
    match outcome {{
        Ok(value) -> {{ return hold(move value) }}
        Error(why) -> {{ return 0 }}
    }}
}}

@Main
function main() {{
    var total = 0
    var i = 0
    while i < 100 {{
        let annotated: Result<Any, AppError> = narrow(i)
        total = total + takes(move annotated)

        var slot: Result<Any, AppError> = narrow(i)
        slot = narrow(i + 1)
        total = total + takes(move slot)

        total = total + takes(narrow(i + 2))

        let elements: [Result<Any, AppError>] = [narrow(i + 3), narrow(0 - 1)]
        total = total + elements.count

        let wrapped = Wrapper(inner: narrow(i + 4))
        total = total + takes(move wrapped.inner)

        let payload: Crate<Result<Any, AppError>> = .Full(narrow(i + 5))
        match payload {{
            Full(held) -> {{ total = total + takes(move held) }}
            Empty -> {{ total = total + 0 }}
        }}
        i = i + 1
    }}
    print(total)
    return
}}
"#
    ));
    // Six crossings an iteration, each answering 2.
    assert_eq!(output, "1200\n");
}
