//! Parity for `match`: arrow arms, arrow-block arms, and payload bindings.
//!
//! A `match` desugars to the `if`/`else` chain every backend already agrees on,
//! so what is actually under test here is the one thing that did *not* desugar:
//! reading a variant's payload. Each backend does that differently — the VM
//! copies a heap `Value`, the native backend calls `kira_rt_enum_payload` and
//! decodes a type-erased word, wasm loads from an offset in a bump-allocated
//! box — and a backend that decoded the word wrong, or handed back a `String`
//! the enum still owned, diverges here or trips the leak check.

use crate::assert_parity;

#[test]
fn arrow_arms_select_the_written_variant() {
    // The corpus `emxAreaOf` shape: arrow arms, every one returning, and *no*
    // trailing `return`. That compiles only because an exhaustive match whose
    // arms all return is itself a definite return — so this case is as much a
    // test of the desugar's shape as of its result.
    let output = assert_parity(
        r#"
enum Shade { Light Mid Dark }

function rank(s: borrow Shade) -> Int {
    match s {
        Light -> return 1;
        Mid -> return 2;
        Dark -> return 3;
    }
}

@Main
function main() {
    let light: Shade = .Light
    let mid: Shade = .Mid
    let dark: Shade = .Dark
    print(rank(light) * 100 + rank(mid) * 10 + rank(dark))
    return
}
"#,
    );
    assert_eq!(output, "123\n");
}

#[test]
fn arrow_block_arms_agree() {
    // The corpus `EmxParseError` shape: arrow-block arms assigning to a `var`
    // declared before the match, rather than returning out of it.
    let output = assert_parity(
        r#"
enum ParseError { InvalidFormat EmptyInput UnexpectedEnd }

function describe(e: borrow ParseError) -> String {
    var out = ""
    match e {
        InvalidFormat -> { out = "invalid" }
        EmptyInput -> { out = "empty" }
        UnexpectedEnd -> { out = "end" }
    }
    return out
}

@Main
function main() {
    let a: ParseError = .EmptyInput
    let b: ParseError = .UnexpectedEnd
    print(describe(a))
    print(describe(b))
    return
}
"#,
    );
    assert_eq!(output, "empty\nend\n");
}

#[test]
fn a_string_payload_binding_agrees() {
    // The binding must own its string: the enum is freed when the frame is, and
    // a backend that handed back the box's own handle would double-free or
    // print freed memory.
    let output = assert_parity(
        r#"
enum Note { Tag(String) Blank }

function textOf(n: borrow Note) -> String {
    match n {
        Tag(text) -> return text;
        Blank -> return "none";
    }
}

@Main
function main() {
    let tagged: Note = .Tag("hello")
    let blank: Note = .Blank
    print(textOf(tagged))
    print(textOf(blank))
    return
}
"#,
    );
    assert_eq!(output, "hello\nnone\n");
}

#[test]
fn scalar_payload_bindings_agree() {
    // Every payload type the declaration admits, round-tripped through the
    // box's one type-erased word. A `Float` goes through a bitcast and a `Bool`
    // through a truncation on the native path, so a wrong width shows up here.
    let output = assert_parity(
        r#"
enum Cell { Count(Int) Ratio(Float) Flag(Bool) Empty }

function score(c: borrow Cell) -> Int {
    match c {
        Count(n) -> return n;
        Ratio(r) -> { if r > 1.5 { return 20 } return 10 }
        Flag(b) -> { if b { return 1 } return 0 }
        Empty -> return 0;
    }
}

@Main
function main() {
    let counted: Cell = .Count(7)
    let ratio: Cell = .Ratio(2.5)
    let flag: Cell = .Flag(true)
    let empty: Cell = .Empty
    print(score(counted) + score(ratio) + score(flag) + score(empty))
    return
}
"#,
    );
    assert_eq!(output, "28\n");
}

#[test]
fn a_payload_binding_in_a_loop_stays_balanced() {
    // Every iteration reads a fresh owned copy of the payload. A backend that
    // leaked one would fail the VM's heap accounting (current == 0 at exit),
    // and one that freed the box's copy would trap on the second iteration.
    let output = assert_parity(
        r#"
enum Note { Tag(String) Blank }

@Main
function main() {
    let tagged: Note = .Tag("ab")
    var out = ""
    var i = 0
    while i < 4 {
        match tagged {
            Tag(text) -> { out = out + text }
            Blank -> { out = out + "?" }
        }
        i = i + 1
    }
    print(out)
    return
}
"#,
    );
    assert_eq!(output, "abababab\n");
}

#[test]
fn a_match_on_a_struct_field_agrees() {
    // The subject is an expression, not just a name, and it must be evaluated
    // exactly once — a field read that ran per arm would still print the same
    // answer here, but the enum it clones would leak on every extra read.
    let output = assert_parity(
        r#"
enum Shade { Light Mid Dark }

struct Swatch { let shade: Shade  let weight: Int }

@Main
function main() {
    let swatch = Swatch { shade: .Mid, weight: 5 }
    var out = 0
    match swatch.shade {
        Light -> { out = swatch.weight }
        Mid -> { out = swatch.weight * 2 }
        Dark -> { out = swatch.weight * 3 }
    }
    print(out)
    return
}
"#,
    );
    assert_eq!(output, "10\n");
}

#[test]
fn a_match_across_the_hybrid_seam_agrees() {
    // The annotated case: `@Runtime` puts `classify` on the VM while `main` is
    // native, so the hybrid build has a real boundary here. The enum and its
    // payload never cross it — only the `Int` result does — which is what makes
    // this legal, and agreeing with the two single-engine builds is the claim
    // that the boundary moved where the match ran and nothing else.
    let output = assert_parity(
        r#"
enum Cell { Count(Int) Empty }

@Runtime
function classify(n: Int) -> Int {
    var c: Cell = .Empty
    if n > 0 { c = .Count(n) }
    match c {
        Count(v) -> return v * 2;
        Empty -> return -1;
    }
}

@Main
function main() {
    print(classify(4))
    print(classify(0))
    return
}
"#,
    );
    assert_eq!(output, "8\n-1\n");
}

#[test]
fn a_match_inside_a_loop_lets_break_reach_the_loop() {
    // A `break` in a match arm belongs to the enclosing loop, exactly as it
    // does in a `switch` arm: the desugar builds `if`s, and an `if` pushes no
    // loop depth. A backend that scoped the break to the match would run
    // forever or fall out one iteration late.
    let output = assert_parity(
        r#"
enum Step { Go Stop }

@Main
function main() {
    var count = 0
    var i = 0
    while i < 10 {
        var s: Step = .Go
        if i == 3 { s = .Stop }
        match s {
            Go -> { count = count + 1 }
            Stop -> { break }
        }
        i = i + 1
    }
    print(count)
    return
}
"#,
    );
    assert_eq!(output, "3\n");
}
