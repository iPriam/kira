//! Parity for `for`, `break`, `continue`, and `switch`.

use crate::assert_parity;

/// A `for` loop over a range. It reaches wasm already desugared to a `while`,
/// so this proves the desugar and the loop lowering agree with the VM.
#[test]
fn for_loops_over_ranges_agree() {
    assert_parity(
        r#"
@Main
function main() {
    var sum = 0
    for i in 0..5 {
        sum = sum + i
    }
    print(sum)

    let lo = 2
    let hi = 6
    var span = 0
    for i in lo..hi {
        span = span + 1
    }
    print(span)

    var ran = 0
    for i in 5..5 {
        ran = ran + 1
    }
    print(ran)

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
}

/// `break` and `continue` become `br` instructions, and wasm names a branch
/// target by how many labels to pop rather than by identity. A jump nested
/// inside an `if` therefore needs a different immediate than the same jump
/// written at the top of the body — so every case here nests one.
#[test]
fn break_and_continue_agree_at_every_nesting_depth() {
    assert_parity(
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

    var found = 0
    for i in 0..100 {
        if i * i > 50 {
            found = i
            break
        }
    }
    print(found)

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
}

/// A jump buried several `if`s deep inside nested loops: the branch immediate
/// is a function of the depth, so this is the case a hand-computed constant
/// gets wrong.
#[test]
fn a_deeply_nested_jump_finds_its_own_loop() {
    assert_parity(
        r#"
@Main
function main() {
    var hits = 0
    for i in 0..6 {
        if i > 0 {
            if i % 2 == 0 {
                if i > 3 {
                    break
                }
                continue
            }
        }
        hits = hits + 1
    }
    print(hits)
    return
}
"#,
    );
}

/// A `switch` reaches wasm already desugared to an `if`/`else` chain, so this
/// proves the desugar agrees with the VM on every arm shape.
#[test]
fn switch_dispatch_agrees() {
    assert_parity(
        r#"
@Main
function main() {
    for i in 0..4 {
        var name = "?"
        switch i % 3 {
            case 0 { name = "zero" }
            case 1 { name = "one" }
            case 2 { name = "two" }
            default { name = "other" }
        }
        print(name)
    }

    var untouched = 7
    switch 99 {
        case 1 { untouched = 1 }
    }
    print(untouched)

    var kind = 0
    switch "beta" {
        case "alpha" { kind = 1 }
        case "beta" { kind = 2 }
        default { kind = 9 }
    }
    print(kind)
    return
}
"#,
    );
}

/// A `break` inside a switch arm inside a loop: the jump target is the loop,
/// and wasm names it by label depth, so the arm's nesting is what would get
/// the immediate wrong.
#[test]
fn break_in_a_switch_arm_finds_the_enclosing_loop() {
    assert_parity(
        r#"
@Main
function main() {
    var seen = 0
    for i in 0..10 {
        switch i {
            case 3 { break }
            default { seen = seen + 1 }
        }
    }
    print(seen)
    return
}
"#,
    );
}
