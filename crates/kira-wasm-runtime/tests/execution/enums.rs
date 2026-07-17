//! Parity for enums: construction, leading-dot members, tag equality, payload
//! variants, and enums inside structs and arrays — VM == wasm32 == wasm64.
//!
//! The observable operation on an enum here is `==`/`!=` — a tag comparison —
//! so every case turns an enum into an `Int` that way. A width-dependent box
//! layout, or a tag read at the wrong offset, would surface as a disagreement
//! between the two address widths.

use crate::assert_parity;

#[test]
fn payload_less_equality_agrees() {
    assert_parity(
        r#"
enum Color { Red Green Blue }

function rank(c: Color) -> Int {
    if c == .Red { return 1 }
    if c == .Green { return 2 }
    return 3
}

@Main
function main() {
    var total = rank(.Red) + rank(.Green) + rank(.Blue)
    let red: Color = .Red
    let green: Color = .Green
    if red != green { total = total + 10 }
    print(total)
    return
}
"#,
    );
}

#[test]
fn an_enum_field_of_a_struct_agrees() {
    assert_parity(
        r#"
enum Color { Red Green Blue }

struct Box { let c: Color }

function is_blue(b: borrow Box) -> Bool {
    return b.c == .Blue
}

@Main
function main() {
    let blue = Box { c: .Blue }
    if is_blue(blue) { print(1) } else { print(0) }
    return
}
"#,
    );
}

#[test]
fn a_string_payload_and_a_default_agree() {
    assert_parity(
        r#"
enum ParseError {
    InvalidFormat: String = "not the expected format"
    UnexpectedEnd
}

function code(e: ParseError) -> Int {
    if e == .InvalidFormat { return 1 }
    return 2
}

@Main
function main() {
    let m: ParseError = .InvalidFormat
    print(code(move m))
    print(code(.UnexpectedEnd))
    return
}
"#,
    );
}

#[test]
fn a_scalar_payload_agrees() {
    assert_parity(
        r#"
enum Dim { Fill Fixed(Float) At(Int) }

function is_fill(d: Dim) -> Int {
    if d == .Fill { return 1 }
    return 0
}

@Main
function main() {
    print(is_fill(.Fill))
    print(is_fill(.Fixed(3.0)))
    print(is_fill(.At(7)))
    return
}
"#,
    );
}

#[test]
fn an_array_of_enums_agrees() {
    assert_parity(
        r#"
enum Color { Red Green Blue }

function rank(c: Color) -> Int {
    if c == .Red { return 1 }
    if c == .Green { return 2 }
    return 3
}

@Main
function main() {
    let colors: [Color] = [.Red, .Green, .Blue]
    var sum = 0
    var i = 0
    while i < colors.count {
        sum = sum + rank(colors[i])
        i = i + 1
    }
    print(sum)
    return
}
"#,
    );
}
