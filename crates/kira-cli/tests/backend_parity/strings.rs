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

/// `s.count` is a string's length in **bytes**, and every backend agrees on it
/// — including for text that is not all ASCII, where a character count would
/// differ.
///
/// The units matter beyond this test: `charAt` and `substring` index the same
/// ones, so a count in characters would disagree with the primitives it sits
/// beside, and with the wire formats built on them.
#[test]
fn a_strings_byte_count_agrees_on_every_backend() {
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
    // `héllo` is 6 bytes (é is two) and `日本語` is 9 (three each).
    assert_eq!(output, "4\n0\n6\n9\n3\n6\nempty\none\nmany\n");
}

/// The four string primitives every wire format is carved with, and the
/// composition that parses an integer back out of text using only them.
#[test]
fn the_string_primitives_agree() {
    let output = assert_parity(
        r#"
function spxPoint(x: Int) -> String {
    return "Point { x: " + String(x) + " }"
}

function spxParseValue(entry: String) -> Int {
    let eq = entry.indexOf("=")
    let digits = entry.substring(eq + 1, entry.count)
    var acc = 0
    var i = 0
    while i < digits.count {
        let d = digits.charAt(i) - 48
        acc = acc * 10 + d
        i = i + 1
    }
    return acc
}

@Main
function main() {
    print(String(0))
    print(String(-42))
    print(String(9223372036854775807))
    print(String(true))
    print(String(false))
    print(String(2.0))
    print(String(0.1))
    print(String(1.0 / 3.0))

    // `.count` is a byte count, so text that is not all ASCII says so.
    print("".count)
    print("hello".count)
    print("café".count)
    print("中文".count)

    print("hello".charAt(0))
    print("hello".charAt(2))
    print("hello".charAt(4))

    print("hello".substring(0, 0))
    print("hello".substring(0, 5))
    print("hello".substring(1, 4))
    let joined = "cat" + "dog"
    print(joined.substring(2, 5))

    print("hello".indexOf("he"))
    print("hello".indexOf("ll"))
    print("hello".indexOf("lo"))
    print("hello".indexOf("z"))
    print("hello".indexOf(""))
    print("hello".indexOf("hello"))

    print(spxPoint(1))
    print(spxParseValue("x=42"))
    print(spxParseValue("count=1024"))
    return
}
"#,
    );
    assert_eq!(
        output,
        "0\n-42\n9223372036854775807\ntrue\nfalse\n2\n0.1\n0.3333333333333333\n\
         0\n5\n5\n6\n\
         104\n108\n111\n\
         \nhello\nell\ntdo\n\
         0\n2\n3\n-1\n0\n0\n\
         Point { x: 1 }\n42\n1024\n"
    );
}

/// Every out-of-range string access traps identically on every backend.
#[test]
fn a_string_index_out_of_range_traps_on_every_backend() {
    for body in [
        "let c = \"hello\".charAt(-1)\n    print(c)",
        "let c = \"hello\".charAt(5)\n    print(c)",
        "let s = \"hello\".substring(2, 1)\n    print(s.count)",
        "let n = \"hello\".count\n    let s = \"hello\".substring(0, n + 1)\n    print(s.count)",
    ] {
        let source = format!("@Main\nfunction main() {{\n    {body}\n    return\n}}\n");
        let output = assert_parity(&source);
        assert_eq!(output, "", "`{body}` produced output instead of trapping");
    }
}

/// The predicates: `contains`, `startsWith` and `endsWith` all answer the same
/// `Bool` on both engines, empty needles and empty receivers included.
#[test]
fn string_predicates_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let text = "hello world"
    print(text.contains("o w"))
    print(text.contains("zebra"))
    print(text.contains(""))
    print(text.startsWith("hello"))
    print(text.startsWith("world"))
    print(text.endsWith("world"))
    print(text.endsWith("hello"))
    print("".contains(""))
    print("".startsWith("x"))
    return
}
"#,
    );
    assert_eq!(
        output,
        "true\nfalse\ntrue\ntrue\nfalse\ntrue\nfalse\ntrue\nfalse\n"
    );
}

/// The text-producing operations. `trim` and the case pair are defined on
/// characters, so a non-ASCII case change must agree rather than each engine
/// going its own way per byte.
#[test]
fn string_rewrites_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print("  padded  ".trim())
    print("".trim())
    print("a-b-c".replace("-", "+"))
    print("aaa".replace("a", ""))
    print("no match".replace("zebra", "x"))
    print("MiXeD".lowercase())
    print("MiXeD".uppercase())
    return
}
"#,
    );
    assert_eq!(output, "padded\n\na+b+c\n\nno match\nmixed\nMIXED\n");
}

/// `split` builds an array, which is the one operation here that allocates a
/// container rather than a string — so both engines must agree on the piece
/// count as well as the pieces, and reclaim every one of them.
#[test]
fn string_split_agrees() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let pieces = "a,b,c".split(",")
    print(pieces.count)
    for piece in pieces {
        print(piece)
    }
    // A separator that never occurs yields the whole text as one piece.
    let whole = "abc".split(",")
    print(whole.count)
    print(whole[0])
    // Adjacent separators yield the empty pieces between them.
    let empties = "a,,b".split(",")
    print(empties.count)
    print(empties[1] == "")
    // An empty separator is not a split of anything, so the text comes back
    // whole rather than one piece per character.
    let unsplit = "abc".split("")
    print(unsplit.count)
    print(unsplit[0])
    return
}
"#,
    );
    assert_eq!(output, "3\na\nb\nc\n1\nabc\n3\ntrue\n1\nabc\n");
}
