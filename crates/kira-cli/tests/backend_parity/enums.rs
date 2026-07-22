//! Parity for enums: payload-less and single-payload construction, leading-dot
//! members, tag equality, defaults, move, and enums inside structs and arrays.
//!
//! The observable operation on an enum in this subset is `==`/`!=` — a tag
//! comparison — so every case turns an enum into an `Int` that way and prints
//! it. A backend that boxed the tag wrong, compared handles instead of tags, or
//! leaked a payload would diverge here or trap on the leak check.

use crate::assert_parity;

#[test]
fn payload_less_equality_agrees() {
    // The corpus `enum_equality_tag_compare` shape: rank by `==`, then `!=`.
    let output = assert_parity(
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
    assert_eq!(output, "16\n");
}

#[test]
fn an_enum_field_of_a_struct_agrees() {
    let output = assert_parity(
        r#"
enum Color { Red Green Blue }

struct Box { let c: Color }

function is_blue(b: borrow Box) -> Bool {
    return b.c == .Blue
}

@Main
function main() {
    let blue = Box { c: .Blue }
    if is_blue(blue) {
        print(100)
    } else {
        print(0)
    }
    return
}
"#,
    );
    assert_eq!(output, "100\n");
}

#[test]
fn reassigning_an_enum_var_agrees() {
    // The corpus `enum_var_reassign_return` shape, reduced to what `==` can
    // observe: a `var` typed by its annotation, reassigned to fresh variants.
    let output = assert_parity(
        r#"
enum Shade { Light Dark Mid }

function weight(s: Shade) -> Int {
    if s == .Light { return 1 }
    if s == .Dark { return 2 }
    return 3
}

function shade_for(i: Int) -> Shade {
    var s: Shade = .Light
    if i % 3 == 1 { s = .Dark }
    if i % 3 == 2 { s = .Mid }
    return s
}

@Main
function main() {
    var acc = 0
    var i = 0
    while i < 6 {
        acc = acc + weight(shade_for(i))
        i = i + 1
    }
    print(acc)
    return
}
"#,
    );
    // i%3 cycles Light(1) Dark(2) Mid(3) twice over 0..6 -> 2*(1+2+3)=12.
    assert_eq!(output, "12\n");
}

#[test]
fn a_string_payload_is_built_moved_and_freed_cleanly() {
    // The payload is not readable without `match`, but it must be built, moved,
    // and freed without leaking — which a clean exit and the VM's heap check
    // prove. The tag comparison stays observable throughout.
    let output = assert_parity(
        r#"
enum Msg { Empty Text(String) }

function code(m: Msg) -> Int {
    if m == .Empty { return 0 }
    return 1
}

@Main
function main() {
    let m: Msg = .Text("hello")
    let moved = move m
    print(code(move moved))
    print(code(.Empty))
    return
}
"#,
    );
    assert_eq!(output, "1\n0\n");
}

#[test]
fn a_payload_default_is_used_when_none_is_written() {
    // `.InvalidFormat` supplies its declared default. The default is a `String`
    // built and freed like any payload; the tag is what stays observable.
    let output = assert_parity(
        r#"
enum ParseError {
    InvalidFormat: String = "not the expected format"
    UnexpectedEnd
    EmptyInput
}

function code(e: ParseError) -> Int {
    if e == .InvalidFormat { return 1 }
    if e == .UnexpectedEnd { return 2 }
    return 3
}

@Main
function main() {
    print(code(.InvalidFormat))
    print(code(.UnexpectedEnd))
    print(code(.EmptyInput))
    return
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn a_scalar_payload_is_built_and_the_tag_compares() {
    let output = assert_parity(
        r#"
enum Dim { Fill Fixed(Float) Bounded(Int) }

function is_fill(d: Dim) -> Int {
    if d == .Fill { return 1 }
    return 0
}

@Main
function main() {
    print(is_fill(.Fill))
    print(is_fill(.Fixed(3.0)))
    print(is_fill(.Bounded(42)))
    return
}
"#,
    );
    assert_eq!(output, "1\n0\n0\n");
}

#[test]
fn an_array_of_enums_agrees() {
    // A leading-dot element resolves against the array's element type, and an
    // element read out of the array is an independent enum whose tag compares.
    let output = assert_parity(
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
    assert_eq!(output, "6\n");
}

#[test]
fn a_leading_dot_return_resolves_against_the_return_type() {
    // `return .Red` has no local annotation to lean on: the leading dot resolves
    // against the function's declared return type. This pins that context by
    // itself rather than leaving it to the plumbing the `let x: EnumType = .V` cases
    // already exercise.
    let output = assert_parity(
        r#"
enum Color { Red Green Blue }

function pick(i: Int) -> Color {
    if i == 0 { return .Red }
    if i == 1 { return .Green }
    return .Blue
}

function rank(c: Color) -> Int {
    if c == .Red { return 1 }
    if c == .Green { return 2 }
    return 3
}

@Main
function main() {
    print(rank(pick(0)) + rank(pick(1)) + rank(pick(2)))
    return
}
"#,
    );
    assert_eq!(output, "6\n");
}

#[test]
fn moving_an_enum_across_a_boundary_agrees() {
    // `@Runtime`/`@Native` split: the enum stays on one side of the seam (it
    // cannot cross), but the program still runs on every backend, hybrid making
    // a real boundary out of the annotations the other two ignore.
    let output = assert_parity(
        r#"
enum Flag { Off On }

@Runtime
function flip(i: Int) -> Int {
    var f: Flag = .Off
    if i % 2 == 0 { f = .On }
    if f == .On { return 1 }
    return 0
}

@Main
function main() {
    print(flip(2))
    print(flip(3))
    return
}
"#,
    );
    assert_eq!(output, "1\n0\n");
}
