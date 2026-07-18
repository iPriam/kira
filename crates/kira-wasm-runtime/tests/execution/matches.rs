//! Parity for `match`: arrow arms and payload bindings — VM == wasm32 ==
//! wasm64.
//!
//! The control flow is a desugar the backends already agreed on, so what is
//! under test is the payload read. On wasm that is a load at a fixed offset in
//! the enum box, and the box's payload slot is the one place an address-width
//! assumption could hide: a `String` payload is an address and a `Float` is
//! not, so a slot sized for one would break the other on exactly one of the two
//! memories.

use crate::assert_parity;

#[test]
fn arrow_arms_select_the_written_variant() {
    assert_parity(
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
}

#[test]
fn arrow_block_arms_agree() {
    assert_parity(
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
}

#[test]
fn a_string_payload_binding_agrees() {
    // A `String` payload is an address, so its slot is where the two memories
    // could disagree — wasm64 stores a wider pointer in the same box.
    assert_parity(
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
}

#[test]
fn scalar_payload_bindings_agree() {
    // An `Int` and a `Float` payload are both 8 bytes on either memory, and a
    // `Bool` is not — the slot has to hold all three whatever the address
    // width.
    assert_parity(
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
}

#[test]
fn a_match_nested_in_control_flow_agrees() {
    // wasm names a branch target by label depth, so a match inside a loop
    // inside an `if` is where a hardcoded immediate would send a jump to the
    // wrong block. The desugar builds `if`s, and `break` must still reach the
    // loop past all of them.
    assert_parity(
        r#"
enum Step { Go Skip Stop }

@Main
function main() {
    var count = 0
    var i = 0
    if i == 0 {
        while i < 10 {
            var s: Step = .Go
            if i == 2 { s = .Skip }
            if i == 5 { s = .Stop }
            match s {
                Go -> { count = count + 1 }
                Skip -> { count = count + 10 }
                Stop -> { break }
            }
            i = i + 1
        }
    }
    print(count)
    return
}
"#,
    );
}

#[test]
fn a_payload_binding_in_a_loop_agrees() {
    assert_parity(
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
}
