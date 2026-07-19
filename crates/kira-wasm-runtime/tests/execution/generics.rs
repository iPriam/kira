//! Parity for generic enums: VM == wasm32 == wasm64.
//!
//! Generics are gone by the time bytecode exists — a generic enum
//! monomorphizes in semantics into an ordinary enum — so what these cases prove
//! is that the enum monomorphization produced is boxed identically at both
//! address widths. A payload whose representation shifted with the pointer size
//! would surface as a disagreement here, exactly as it would for a
//! hand-written enum.

use crate::assert_parity;

/// The oracle's `Result` verbatim.
const RESULT: &str = "
enum Result<Value, Failure> {
    Ok(Value)
    Error(Failure)
}
";

#[test]
fn the_oracles_result_agrees_at_both_widths() {
    assert_parity(&format!(
        r#"
enum AppError {{ NotFound Denied }}
{RESULT}
function find(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.NotFound) }}
    if n > 100 {{ return .Error(.Denied) }}
    return .Ok(n * 2)
}}

function describe(n: Int) -> Int {{
    match find(n) {{
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
}

#[test]
fn two_instantiations_carry_their_own_payload_types() {
    assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
{RESULT}
function number() -> Result<Int, AppError> {{ return .Ok(7) }}
function text() -> Result<String, AppError> {{ return .Ok("seven") }}

@Main
function main() {{
    match number() {{
        Ok(n) -> {{ print(n) }}
        Error -> {{ print(0) }}
    }}
    match text() {{
        Ok(s) -> {{ print(s) }}
        Error -> {{ print("none") }}
    }}
    return
}}
"#
    ));
}

#[test]
fn a_nested_instantiation_agrees_at_both_widths() {
    assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
{RESULT}
function inner() -> Result<Int, AppError> {{ return .Ok(3) }}

function outer(flag: Bool) -> Result<Result<Int, AppError>, AppError> {{
    if flag {{ return .Ok(inner()) }}
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
}

#[test]
fn a_generic_result_flows_through_attempt_and_try() {
    assert_parity(&format!(
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
}
