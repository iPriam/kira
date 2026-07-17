//! Parity for recursion, `if`/`while`, `for`, `break`, and `continue`.

use crate::assert_parity;

#[test]
fn recursion_and_control_flow_agree() {
    let output = assert_parity(
        r#"
@Main
function main() {
    print(fib(20))
    var i = 0
    var sum = 0
    while i < 10 {
        sum = sum + i
        i = i + 1
    }
    print(sum)
    if sum > 40 {
        print(sum > 40 && sum < 50)
    } else {
        print(false)
    }
    return
}

function fib(n: Int) -> Int {
    if n < 2 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}
"#,
    );
    assert_eq!(output, "6765\n45\ntrue\n");
}

/// A `for` is desugared to a `while` in the analyzer, so what this really
/// proves is that the desugar is right — every backend below it is compiling
/// a loop it already had.
#[test]
fn a_for_loop_over_a_range_agrees() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var sum = 0
    for i in 0..5 {
        sum = sum + i
    }
    print(sum)

    // Computed bounds, not just literals.
    let lo = 2
    let hi = 6
    var span = 0
    for i in lo..hi {
        span = span + 1
    }
    print(span)

    // Nested, to prove each loop keeps its own cursor.
    var cells = 0
    for i in 0..3 {
        for j in 0..4 {
            cells = cells + 1
        }
    }
    print(cells)
    return
}
"#,
    );
    assert_eq!(output, "10\n4\n12\n");
}

/// The range is half-open, matching the language: `for i in 5..5` runs zero
/// times and a descending range never runs at all.
#[test]
fn an_empty_for_range_never_runs() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var ran = 0
    for i in 5..5 {
        ran = ran + 1
    }
    print(ran)
    for i in 9..2 {
        ran = ran + 100
    }
    print(ran)
    print(last())
    return
}

function last() -> Int {
    var seen = 0
    for i in 0..4 {
        seen = i
    }
    return seen
}
"#,
    );
    // The last value a `0..4` loop sees is 3, not 4 — the end is excluded.
    assert_eq!(output, "0\n0\n3\n");
}

/// `continue` must not skip the loop's step. A desugar that increments at the
/// end of the body instead of the start hangs here rather than failing, which
/// is why the case exists.
#[test]
fn continue_still_advances_a_for_loop() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var odds = 0
    for i in 0..10 {
        if i % 2 == 0 {
            continue
        }
        odds = odds + i
    }
    print(odds)

    var w = 0
    var i = 0
    while i < 10 {
        i = i + 1
        if i % 2 == 0 {
            continue
        }
        w = w + i
    }
    print(w)
    return
}
"#,
    );
    assert_eq!(output, "25\n25\n");
}

/// `break` leaves the innermost loop only, and statements after it never run.
#[test]
fn break_leaves_only_the_innermost_loop() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var found = 0
    for i in 0..100 {
        if i * i > 50 {
            found = i
            break
        }
    }
    print(found)

    // The inner `break` must not escape the outer loop.
    var pairs = 0
    for i in 0..3 {
        for j in 0..3 {
            if j > i {
                break
            }
            pairs = pairs + 1
        }
    }
    print(pairs)

    // A `break` nested inside an `if` inside a `while`: the wasm and LLVM
    // backends name their jump targets by depth, so nesting is what would
    // break them.
    var guard = 0
    var total = 0
    while true {
        guard = guard + 1
        if guard > 3 {
            break
        }
        total = total + guard
    }
    print(total)
    return
}
"#,
    );
    assert_eq!(output, "8\n6\n6\n");
}

/// A `for` body that rebinds a string every iteration: the loop variable is a
/// fresh binding per iteration, and the slot it stores into must be reclaimed
/// rather than leaked or double-freed.
#[test]
fn a_for_loop_body_reclaims_its_bindings() {
    let output = assert_parity(
        r#"
@Main
function main() {
    var last = ""
    for i in 0..4 {
        let label = "row " + name(i)
        last = label
    }
    print(last)
    return
}

function name(n: Int) -> String {
    if n == 0 {
        return "zero"
    }
    if n == 1 {
        return "one"
    }
    if n == 2 {
        return "two"
    }
    return "three"
}
"#,
    );
    assert_eq!(output, "row three\n");
}
