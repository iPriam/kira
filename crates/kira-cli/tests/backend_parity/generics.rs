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

use crate::{assert_module_parity, assert_parity};

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
fn a_generic_enum_round_trips_a_payload_of_every_supported_kind() {
    // The payload kinds an enum may carry are `Int`, `Float`, `Bool`, `String`,
    // a struct, and another enum. Each one is a different representation — an
    // immediate, a float register, a heap handle, an aggregate — so this is the
    // case that would catch monomorphization producing a row whose payload one
    // engine reads back differently from another.
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound }}
enum Shade {{ Dim Bright }}
{RESULT}
struct Pt {{
    let x: Int
    let y: Int
}}

@Main
function main() {{
    let number: Result<Int, AppError> = .Ok(42)
    match number {{ Ok(v) -> {{ print(v) }} Error -> {{ print(0) }} }}

    let real: Result<Float, AppError> = .Ok(1.5)
    match real {{ Ok(v) -> {{ print(v) }} Error -> {{ print(0.0) }} }}

    let flag: Result<Bool, AppError> = .Ok(true)
    match flag {{ Ok(v) -> {{ print(v) }} Error -> {{ print(false) }} }}

    let text: Result<String, AppError> = .Ok("payload")
    match text {{ Ok(v) -> {{ print(v) }} Error -> {{ print("none") }} }}

    let point: Result<Pt, AppError> = .Ok(Pt {{ x = 3, y = 4 }})
    match point {{ Ok(v) -> {{ print(v.x + v.y) }} Error -> {{ print(0) }} }}

    let inner: Result<Shade, AppError> = .Ok(.Bright)
    match inner {{
        Ok(v) -> {{ if v == .Bright {{ print(1) }} else {{ print(0) }} }}
        Error -> {{ print(0 - 1) }}
    }}

    let failed: Result<Int, AppError> = .Error(.NotFound)
    match failed {{ Ok(v) -> {{ print(v) }} Error -> {{ print(0 - 9) }} }}
    return
}}
"#
    ));
    assert_eq!(output, "42\n1.5\ntrue\npayload\n7\n1\n-9\n");
}

#[test]
fn a_qualified_spelling_builds_the_same_value_a_leading_dot_does() {
    // `Result.Ok(1)` carries no type arguments, so the position supplies them —
    // and what it builds has to be the very same value `.Ok(1)` builds, on
    // every engine. A backend seeing a difference here would mean the qualified
    // path reached a different row.
    let output = assert_parity(&format!(
        r#"
enum AppError {{ NotFound Denied }}
{RESULT}
function viaDot(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return .Error(.Denied) }}
    return .Ok(n)
}}

function viaQualified(n: Int) -> Result<Int, AppError> {{
    if n < 0 {{ return Result.Error(.Denied) }}
    return Result.Ok(n)
}}

function unwrap(outcome: Result<Int, AppError>) -> Int {{
    match outcome {{
        Ok(v) -> {{ return v }}
        Error -> {{ return 0 - 1 }}
    }}
}}

@Main
function main() {{
    print(unwrap(viaDot(7)))
    print(unwrap(viaQualified(7)))
    print(unwrap(viaDot(0 - 3)))
    print(unwrap(viaQualified(0 - 3)))

    let text: Result<String, AppError> = Result.Ok("qualified")
    match text {{ Ok(s) -> {{ print(s) }} Error -> {{ print("none") }} }}
    return
}}
"#
    ));
    assert_eq!(output, "7\n7\n-1\n-1\nqualified\n");
}

#[test]
fn a_template_declared_in_another_module_instantiates_and_runs() {
    // The template lives in one file and every instantiation of it in another,
    // so the substitution has to resolve the template's body against the file
    // that *wrote* it while the arguments come from the use site.
    let output = assert_module_parity(
        r#"
import outcome

@Main
function main() {
    let ok: Result<Int, Trouble> = .Ok(11)
    match ok { Ok(v) -> { print(v) } Error -> { print(0) } }

    let qualified: Result<Int, Trouble> = Result.Ok(22)
    match qualified { Ok(v) -> { print(v) } Error -> { print(0) } }

    let bad: Result<String, Trouble> = outcome.Result.Error(.Late)
    match bad { Ok(s) -> { print(s) } Error -> { print("late") } }
    return
}
"#,
        &[(
            "outcome",
            r#"
enum Trouble { Missing Late }

enum Result<Value, Failure> {
    Ok(Value)
    Error(Failure)
}
"#,
        )],
    );
    assert_eq!(output, "11\n22\nlate\n");
}

#[test]
fn foundations_result_instantiates_and_runs_from_an_importing_program() {
    // The premise the whole feature exists for: `Result` is Foundation's, not
    // the program's, and an `import Foundation` is all it takes to name it in
    // type position and construct it. The parity harness pins Foundation to
    // this checkout, so this is about the `Result` in the tree.
    let output = assert_parity(
        r#"
import Foundation

enum Trouble { Missing }

function parse(n: Int) -> Result<Int, Trouble> {
    if n < 0 { return Result.Error(.Missing) }
    return Result.Ok(n * 2)
}

@Main
function main() {
    match parse(4) { Ok(v) -> { print(v) } Error -> { print(0 - 1) } }
    match parse(0 - 4) { Ok(v) -> { print(v) } Error -> { print(0 - 1) } }

    let text: Result<String, Trouble> = .Ok("foundation")
    match text { Ok(s) -> { print(s) } Error -> { print("none") } }
    return
}
"#,
    );
    assert_eq!(output, "8\n-1\nfoundation\n");
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

#[test]
fn generic_aggregates_traits_and_functions_agree() {
    // Structs and classes become ordinary struct rows, while a generic trait
    // becomes an ordinary conformance contract and a generic function becomes
    // an ordinary callable. This exercises all four monomorphization paths in
    // one VM/LLVM/hybrid run, including inference through a nested constructor.
    let output = assert_parity(
        r#"
trait Scored {
    function score(borrow self) -> Int
}

struct Score: Scored {
    let value: Int
    function score(borrow self) -> Int { return self.value }
}

trait Provider<Value: Scored> {
    function get(borrow self) -> Value
    function scoreValue(borrow self) -> Int { return self.get().score() }
}

struct ProviderBox: Provider<Score> {
    let value: Score
    function get(borrow self) -> Score { return self.value }
}

struct Box<Value: Scored> {
    let value: Value
    function score(borrow self) -> Int { return self.value.score() }
}

class Holder<Value: Scored> {
    let value: Value
    function score(borrow self) -> Int { return self.value.score() }
}

function identity<Value>(value: Value) -> Value { return value }
function unbox<Value: Scored>(value: Box<Value>) -> Value { return value.value }

@Main
function main() {
    let boxed = Box(value: Score(value: 5))
    let held = Holder(Score(value: 7))
    let inferred = unbox(Box(value: Score(value: 9)))
    let provider = ProviderBox(value: Score(value: 10))
    print(boxed.score())
    print(held.score())
    print(inferred.score())
    print(provider.scoreValue())
    print(identity(11))
    print(identity<Int>(12))
    return
}
"#,
    );
    assert_eq!(output, "5\n7\n9\n10\n11\n12\n");
}

#[test]
fn parameterized_generic_class_inheritance_agrees() {
    let output = assert_parity(
        r#"
class Grand<Value> {
    let value: Value
    function get(borrow self) -> Value { return self.value }
}

class Parent<Value> extends Grand<Value> {}

class Child<Value> extends Parent<Value> {
    override function get(borrow self) -> Value { return self.value }
}

@Main
function main() {
    let child = Child(value: 17)
    print(child.get())
    return
}
"#,
    );
    assert_eq!(output, "17\n");
}
