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
