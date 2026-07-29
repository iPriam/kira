//! Parity for shared mutable `var` captures: the VM, the LLVM/native backend,
//! and the hybrid bundle must agree.
//!
//! A captured `var` is the first genuinely shared, mutable storage in the
//! language — everything else here has value semantics — so these cases are
//! about *who sees whose write*, which is the one question a copy would answer
//! differently while still running.
//!
//! # Why every case loops
//!
//! Each program repeats its interaction one to two hundred times. A cell is
//! share-counted, and a count that is one too low frees the box while a holder
//! still names it, while one too high leaks quietly. Repeating the interaction
//! turns the first into a crash or a wrong answer instead of a run that happens
//! to survive one pass, and it is the difference between these tests proving
//! the accounting and merely exercising it.
//!
//! The counts also *accumulate*: every case prints a total, so a single missed
//! write in two hundred is a different number rather than a rounding error.

use crate::assert_parity;

#[test]
fn a_closure_write_is_visible_to_the_scope_that_declared_the_var() {
    // The base case, and the one the whole feature is for: the closure and the
    // frame that declared `total` name one storage. A capture by copy runs and
    // prints 0.
    let output = assert_parity(
        r#"
@Main
function main() {
    var total = 0
    let add: (Int) -> Int = { value in
        total = total + value
        return total
    }
    var i = 1
    while i <= 200 {
        let _ = add(i)
        i = i + 1
    }
    print(total)
    return
}
"#,
    );
    // 1 + 2 + … + 200
    assert_eq!(output, "20100\n");
}

#[test]
fn two_closures_share_one_captured_var() {
    // Two literals, one box. Each closure's writes have to be visible to the
    // other and to `main`, so a per-closure copy gives three different answers
    // instead of one.
    let output = assert_parity(
        r#"
function callBoth(f: borrow () -> Void, g: borrow () -> Void) {
    f()
    g()
    return
}

@Main
function main() {
    var shared = 0
    let bump: () -> Void = { in
        shared = shared + 1
        return
    }
    let leap: () -> Void = { in
        shared = shared + 10
        return
    }
    var i = 0
    while i < 200 {
        callBoth(bump, leap)
        i = i + 1
    }
    print(shared)
    return
}
"#,
    );
    // 200 × (1 + 10)
    assert_eq!(output, "2200\n");
}

#[test]
fn nested_closures_share_by_lexical_scope() {
    // The inner closure captures through the outer one rather than reaching
    // past it, so all three frames write the same box. It is also what catches
    // two nested literals of one function type taking the same dispatcher tag:
    // with that confusion, `outer` never runs at all and the total is zero.
    let output = assert_parity(
        r#"
@Main
function main() {
    var depth = 0
    let outer: () -> Void = { in
        depth = depth + 1
        let inner: () -> Void = { in
            depth = depth + 100
            return
        }
        inner()
        return
    }
    var i = 0
    while i < 200 {
        outer()
        i = i + 1
    }
    print(depth)
    return
}
"#,
    );
    // 200 × (1 + 100)
    assert_eq!(output, "20200\n");
}

#[test]
fn a_captured_array_is_written_through_the_cell() {
    // The write-back case. Reading the array out of the box hands back a second
    // handle on one block; the element write buys elements of its own; storing
    // the handle back is what makes the new block the one the box holds.
    // Without the store-back, every write lands in a copy and the array never
    // changes.
    let output = assert_parity(
        r#"
@Main
function main() {
    var xs = [1, 2, 3]
    let poke: (Int) -> Void = { value in
        xs[0] = xs[0] + value
        xs.append(value)
        return
    }
    var i = 0
    while i < 200 {
        poke(1)
        i = i + 1
    }
    print(xs[0])
    print(xs.count)
    print(xs[202])
    return
}
"#,
    );
    // 1 + 200 writes, three original elements plus 200 appended.
    assert_eq!(output, "201\n203\n1\n");
}

#[test]
fn a_captured_struct_is_written_through_the_cell() {
    // The same store-back, for a value that is wider than the box's payload
    // word and so travels out of line. The `String` field is there on purpose:
    // it is what a release of the replaced payload has to reclaim, so a missed
    // release leaks two hundred strings and a doubled one is a double free.
    let output = assert_parity(
        r#"
struct Tally {
    var hits: Int
    var mark: String
}

@Main
function main() {
    var tally = Tally { hits = 0, mark = "" }
    let record: (Int) -> Void = { value in
        tally.hits = tally.hits + value
        tally.mark = tally.mark + "x"
        return
    }
    var i = 0
    while i < 200 {
        record(2)
        i = i + 1
    }
    print(tally.hits)
    print(tally.mark.count)
    return
}
"#,
    );
    assert_eq!(output, "400\n200\n");
}

#[test]
fn a_captured_string_is_replaced_two_hundred_times() {
    // A payload the box owns outright, replaced on every turn. Each write
    // releases the previous string, so this is where a `cell_set` that stored
    // without releasing — or released without storing — shows up.
    let output = assert_parity(
        r#"
@Main
function main() {
    var word = "a"
    let grow: () -> Void = { in
        word = word + "b"
        return
    }
    var i = 0
    while i < 200 {
        grow()
        i = i + 1
    }
    print(word.count)
    return
}
"#,
    );
    assert_eq!(output, "201\n");
}

#[test]
fn a_closure_outlives_the_frame_that_declared_its_var() {
    // The escape case: the box outlives `makeCounter`'s frame because the
    // returned closure holds a share of it. Frame-local storage is a
    // use-after-free here on every backend, and two hundred calls after the
    // frame is gone is what makes that fail rather than survive.
    let output = assert_parity(
        r#"
function makeCounter(): () -> Int {
    var seen = 0
    return { in
        seen = seen + 1
        return seen
    }
}

@Main
function main() {
    let tick = makeCounter()
    var last = 0
    var i = 0
    while i < 200 {
        last = tick()
        i = i + 1
    }
    print(last)
    return
}
"#,
    );
    assert_eq!(output, "200\n");
}

#[test]
fn two_escaped_closures_carry_separate_boxes() {
    // One literal, two calls, two boxes. The declaration allocates per
    // execution, so the counters are independent — a box hoisted to the
    // function rather than the frame would make them one and print 400 twice.
    let output = assert_parity(
        r#"
function makeCounter(): () -> Int {
    var seen = 0
    return { in
        seen = seen + 1
        return seen
    }
}

@Main
function main() {
    let first = makeCounter()
    let second = makeCounter()
    var i = 0
    while i < 200 {
        let _ = first()
        i = i + 1
    }
    var j = 0
    while j < 100 {
        let _ = second()
        j = j + 1
    }
    print(first())
    print(second())
    return
}
"#,
    );
    assert_eq!(output, "201\n101\n");
}

#[test]
fn a_var_declared_in_a_loop_is_a_fresh_box_each_turn() {
    // A closure made on one turn keeps that turn's storage. One box per
    // execution of the declaration is what makes the totals independent; one
    // box per *declaration* would accumulate across turns and print 200.
    let output = assert_parity(
        r#"
@Main
function main() {
    var grand = 0
    var i = 0
    while i < 200 {
        var turn = 0
        let step: () -> Void = { in
            turn = turn + 1
            return
        }
        step()
        step()
        grand = grand + turn
        i = i + 1
    }
    print(grand)
    return
}
"#,
    );
    // Two steps per turn, 200 turns, and no turn seeing another's.
    assert_eq!(output, "400\n");
}

#[test]
fn a_captured_var_and_a_captured_let_travel_together() {
    // The mixed case: one literal capturing a shared box and a copied scalar.
    // The copy must stay a copy — reassigning `base` after the closure is built
    // changes nothing the closure sees — while the box stays shared.
    let output = assert_parity(
        r#"
@Main
function main() {
    let base = 7
    var total = 0
    let add: (Int) -> Void = { value in
        total = total + value + base
        return
    }
    var i = 0
    while i < 100 {
        add(1)
        i = i + 1
    }
    print(total)
    return
}
"#,
    );
    // 100 × (1 + 7)
    assert_eq!(output, "800\n");
}

#[test]
fn a_captured_var_is_read_and_written_through_a_trailing_callback() {
    // The shape the oracle's corpus writes: a callback handed to a function
    // that calls it, mutating a binding of the caller's frame. The callback
    // runs inside `each`, so the write crosses a call boundary in both
    // directions.
    let output = assert_parity(
        r#"
function each(count: Int, handler: borrow (Int) -> Void) {
    var i = 0
    while i < count {
        handler(i)
        i = i + 1
    }
    return
}

@Main
function main() {
    var seen = 0
    var sum = 0
    each(200) { index in
        seen = seen + 1
        sum = sum + index
        return
    }
    print(seen)
    print(sum)
    return
}
"#,
    );
    // 200 calls; 0 + 1 + … + 199.
    assert_eq!(output, "200\n19900\n");
}
