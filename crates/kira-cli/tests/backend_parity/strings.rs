//! Parity for string concatenation, comparison, and reclamation.

use crate::assert_parity;

#[test]
fn strings_concatenate_compare_and_return_identically() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let greeting = "hello"
    let subject = "kira"
    print(greeting + " " + subject)
    print(banner())
    print(greeting == "hello")
    print(greeting == subject)
    print(greeting != subject)
    print("")
    return
}

function banner() -> String {
    return "one source" + ", many backends"
}
"#,
    );
    assert_eq!(
        output,
        "hello kira\none source, many backends\ntrue\nfalse\ntrue\n\n"
    );
}

/// A `let` inside a loop stores into the same slot every iteration; both
/// backends must reclaim the previous value rather than leak or double-free it.
#[test]
fn strings_rebound_in_a_loop_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var i = 0
    var acc = ""
    while i < 3 {
        let piece = "x"
        acc = acc + piece
        i = i + 1
    }
    print(acc)
    return
}
"#,
    );
    assert_eq!(output, "xxx\n");
}

/// `s.count` is a string's **character** count, and every backend agrees on it
/// — including for text that is not all ASCII, where a byte count would differ.
///
/// The units matter beyond this test: `charAt` and `substring` index the same
/// ones, so a count in bytes would disagree with the primitives it sits beside.
#[test]
fn a_strings_character_count_agrees_on_every_backend() {
    let output = assert_parity(
        r#"
function describe(text: borrow String) -> String {
    if text.count == 0 {
        return "empty"
    }
    if text.count == 1 {
        return "one"
    }
    return "many"
}

@Main
function main() {
    print("kira".count)
    print("".count)
    print("héllo".count)
    print("日本語".count)
    let held = "abc"
    print(held.count)
    // Reading it twice proves the count does not consume the binding.
    print(held.count + held.count)
    print(describe(""))
    print(describe("x"))
    print(describe("xy"))
    return
}
"#,
    );
    assert_eq!(output, "4\n0\n5\n3\n3\n6\nempty\none\nmany\n");
}
