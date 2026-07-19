//! Parity for generic enums: VM == LLVM == hybrid.
//!
//! A generic enum monomorphizes in semantics into an ordinary [`EnumDef`], so
//! no backend learns that generics exist and none of them *can* disagree about
//! the feature itself. What these cases actually prove is that claim: the
//! programs below are written generically and must behave exactly like the
//! hand-written enums they become, on every engine, with the same payload
//! reads, the same moves, and the same reclamation the enum tests already pin.
//!
//! A divergence here would mean monomorphization produced something the
//! backends do not agree on — a payload type one of them boxes differently, or
//! a variant order that shifted — which is the only way this feature could
//! reach a backend at all.
//!
//! [`EnumDef`]: kira_semantics_model::EnumDef

use crate::assert_parity;

/// The oracle's `Result` verbatim.
const RESULT: &str = "
enum Result<Value, Failure> {
    Ok(Value)
    Error(Failure)
}
";

#[test]
fn the_oracles_result_agrees_on_every_backend() {
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound Denied }}
{RESULT}
function find(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    if n > 100 {{ return .Error(.Denied) }}
    return .Ok(n * 2)
}}

function describe(n: Int) -> Int {{
    let outcome = find(n)
    match outcome {{
        Ok(value) -> {{ return value }}
        Error(why) -> {{
            if why == .NotFound {{ return 0 - 1 }}
            return 0 - 2
        }}
    }}
}}

@Main
function main() {{
    print(describe(21))
    print(describe(0 - 5))
    print(describe(500))
    return
}}
"#
    ));
    assert_eq!(output, "42\n-1\n-2\n");
}

#[test]
fn two_instantiations_of_one_template_are_two_enums() {
    // `Result<Int, E>` and `Result<String, E>` are separate rows in the same
    // table with differently-typed payloads. A backend that keyed a payload's
    // representation off the *template* rather than the instantiation would
    // read an `Int` out of a string handle here.
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
{RESULT}
function number(flag: Bool) -> Result<Int, AppError> {{
    if flag {{ return .Ok(7) }}
    return .Error(.NotFound)
}}

function text(flag: Bool) -> Result<String, AppError> {{
    if flag {{ return .Ok("seven") }}
    return .Error(.NotFound)
}}

@Main
function main() {{
    match number(true) {{
        Ok(n) -> {{ print(n) }}
        Error -> {{ print(0) }}
    }}
    match text(true) {{
        Ok(s) -> {{ print(s) }}
        Error -> {{ print("none") }}
    }}
    match text(false) {{
        Ok(s) -> {{ print(s) }}
        Error -> {{ print("none") }}
    }}
    return
}}
"#
    ));
    assert_eq!(output, "7\nseven\nnone\n");
}

#[test]
fn a_generic_result_flows_through_attempt_and_try() {
    // The `Result`-shaped check in `attempt` is structural, and a monomorphized
    // `Result<Int, AppError>` satisfies it — so the oracle's own `Result` is
    // what `try` unwraps here, with a nested enum payload each backend has to
    // reclaim.
    let output = assert_parity(&format!(
        r#"
enum ClampError {{ TooSmall TooBig }}
{RESULT}
function clamp(n: Int) -> Result<Int, ClampError> {{
    if n < 0 {{ return .Error(.TooSmall) }}
    if n > 100 {{ return .Error(.TooBig) }}
    return .Ok(n)
}}

function process(n: Int) -> Int {{
    attempt {{
        let v = try clamp(n)
        return v * 2
    }} handle {{
        TooSmall {{ return 0 - 1 }}
        TooBig {{ return 0 - 2 }}
    }}
}}

@Main
function main() {{
    print(process(50))
    print(process(0 - 5))
    print(process(200))
    return
}}
"#
    ));
    assert_eq!(output, "100\n-1\n-2\n");
}

#[test]
fn a_nested_instantiation_agrees() {
    // `Result<Result<Int, E>, E>` closes on a `>>` the parser splits, and puts
    // one heap enum inside another's payload — the recursive-reclaim path.
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
{RESULT}
function inner(flag: Bool) -> Result<Int, AppError> {{
    if flag {{ return .Ok(3) }}
    return .Error(.NotFound)
}}

function outer(flag: Bool) -> Result<Result<Int, AppError>, AppError> {{
    if flag {{ return .Ok(inner(true)) }}
    return .Error(.NotFound)
}}

@Main
function main() {{
    match outer(true) {{
        Ok(held) -> {{
            match held {{
                Ok(n) -> {{ print(n) }}
                Error -> {{ print(0) }}
            }}
        }}
        Error -> {{ print(0 - 1) }}
    }}
    match outer(false) {{
        Ok(held) -> {{ print(99) }}
        Error -> {{ print(0 - 1) }}
    }}
    return
}}
"#
    ));
    assert_eq!(output, "3\n-1\n");
}

#[test]
fn a_generic_instantiation_inside_a_struct_and_an_array_agrees() {
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
{RESULT}
struct Record {{
    let outcome: Result<Int, AppError>
    let weight: Int
}}

@Main
function main() {{
    let held: Result<Int, AppError> = .Ok(5)
    let record = Record {{ outcome: move held, weight: 2 }}
    match record.outcome {{
        Ok(n) -> {{ print(n * record.weight) }}
        Error -> {{ print(0) }}
    }}

    let first: Result<Int, AppError> = .Ok(1)
    let second: Result<Int, AppError> = .Error(.NotFound)
    let all = [move first, move second]
    var total = 0
    for item in all {{
        match item {{
            Ok(n) -> {{ total = total + n }}
            Error -> {{ total = total + 100 }}
        }}
    }}
    print(total)
    return
}}
"#
    ));
    assert_eq!(output, "10\n101\n");
}
