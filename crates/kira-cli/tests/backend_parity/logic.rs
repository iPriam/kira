//! Parity for booleans and the short-circuit operators.

use crate::assert_parity;

#[test]
fn booleans_and_short_circuit_operators_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let yes = true
    let no = false
    print(yes && no)
    print(yes || no)
    print(!yes)
    print(yes == true)
    print(1 < 2 && 3 >= 3)
    return
}
"#,
    );
    assert_eq!(output, "false\ntrue\nfalse\ntrue\ntrue\n");
}

/// `&&` must not evaluate its right operand when the left already decides the
/// answer: the call would trap, so reaching it changes the exit status on
/// whichever backend got it wrong.
#[test]
fn short_circuit_skips_the_right_operand() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var zero = 0
    if false && boom(zero) {
        print(999)
    }
    print(1)
    return
}

function boom(zero: Int) -> Bool {
    return 1 / zero == 0
}
"#,
    );
    assert_eq!(output, "1\n", "the trapping operand must never run");
}
