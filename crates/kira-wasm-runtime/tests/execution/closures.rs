//! Parity for closures: the VM and both wasm word sizes must agree.
//!
//! A closure reaches this crate as a struct plus two ordinary calls — the
//! desugar happens in semantics — so the wasm lowering has no closure-specific
//! path, no new node in the depth walkers, and no call table. These cases are
//! what proves it: had a closure needed an indirect call, it would have had to
//! be lowered here, and it was not.

use crate::assert_parity;

#[test]
fn a_capture_travels_in_the_closure_value() {
    assert_parity(
        r#"
function apply(f: borrow (Int) -> Int, x: Int) -> Int {
    return f(x)
}

@Main
function main() {
    let offset = 100
    let shift: (Int) -> Int = { value in
        return value + offset
    }
    print(shift(1))
    print(apply(shift, 5))
    return
}
"#,
    );
}

#[test]
fn a_closure_returned_from_a_function_keeps_its_capture() {
    assert_parity(
        r#"
function makeAdder(step: Int): (Int) -> Int {
    return { value in
        return value + step
    }
}

@Main
function main() {
    // Bound before it is called: a closure is called by *naming* it, which is
    // the only form the corpus pins — `f(1)(2)` is not written anywhere.
    let add3 = makeAdder(3)
    let add9 = makeAdder(9)
    print(add3(10))
    print(add9(10))
    return
}
"#,
    );
}

#[test]
fn distinct_literals_of_one_type_dispatch_to_their_own_bodies() {
    assert_parity(
        r#"
function invoke(f: borrow (Int) -> Int, x: Int) -> Int {
    return f(x)
}

@Main
function main() {
    let a = 10
    let first: (Int) -> Int = { v in return v + a }
    let b = 100
    let second: (Int) -> Int = { v in return v * b }
    print(invoke(first, 1))
    print(invoke(second, 2))
    return
}
"#,
    );
}

#[test]
fn a_trailing_closure_and_a_zero_parameter_closure_agree() {
    assert_parity(
        r#"
function register(handler: (Int) -> Int) -> Int {
    return handler(2)
}

function run(handler: () -> Int) -> Int {
    return handler()
}

@Main
function main() {
    let base = 30
    print(register { value in
        return base + value
    })
    print(run { in
        return base
    })
    return
}
"#,
    );
}

#[test]
fn a_nested_closure_captures_through_the_one_between() {
    assert_parity(
        r#"
function register(handler: (Int) -> Int) -> Int {
    return handler(2)
}

function run(handler: () -> Int) -> Int {
    return handler()
}

@Main
function main() {
    let base = 30
    print(register { value in
        let inner = value
        return run { in
            return base + inner
        }
    })
    return
}
"#,
    );
}

#[test]
fn a_closure_in_a_field_is_called_through_it() {
    assert_parity(
        r#"
class HandlerBox {
    let onDone: () -> Int
    let onValue: (Int) -> Int
}

function trigger(box: borrow HandlerBox) -> Int {
    return box.onDone() + box.onValue(42)
}

@Main
function main() {
    let done: () -> Int = { in return 1 }
    let value: (Int) -> Int = { v in return v * 2 }
    let box: HandlerBox = HandlerBox(move done, move value)
    print(trigger(box))
    return
}
"#,
    );
}

#[test]
fn a_void_closure_runs_for_effect() {
    assert_parity(
        r#"
function each(handler: (Int) -> Void) {
    handler(1)
    handler(2)
    return
}

@Main
function main() {
    let step = 10
    each { value in
        print(value * step)
    }
    return
}
"#,
    );
}

#[test]
fn a_float_capture_survives_both_address_widths() {
    // A capture rides in a struct field, so the value crosses the same layout
    // an ordinary `Float` field does — on 32-bit and 64-bit memory alike.
    assert_parity(
        r#"
function apply(f: borrow (Float) -> Float, x: Float) -> Float {
    return f(x)
}

@Main
function main() {
    let scale = 2.5
    let stretch: (Float) -> Float = { v in return v * scale }
    print(apply(stretch, 4.0))
    return
}
"#,
    );
}

#[test]
fn a_bool_capture_survives_both_address_widths() {
    assert_parity(
        r#"
function apply(f: borrow (Int) -> Int, x: Int) -> Int {
    return f(x)
}

@Main
function main() {
    let inverted = true
    let pick: (Int) -> Int = { v in
        if inverted {
            return 0 - v
        }
        return v
    }
    print(apply(pick, 7))
    return
}
"#,
    );
}

#[test]
fn a_closure_reclaims_every_allocation_it_makes() {
    // A closure value is a heap struct, copied on every read and dropped on
    // every consume like any other. Output parity would still pass if one of
    // those copies leaked, so the balance is asserted separately.
    crate::assert_heap_balanced(
        r#"
function apply(f: borrow (Int) -> Int, x: Int) -> Int {
    return f(x)
}

function makeAdder(step: Int): (Int) -> Int {
    return { value in
        return value + step
    }
}

@Main
function main() {
    var total = 0
    for i in 0..20 {
        let add = makeAdder(i)
        total = total + apply(add, i)
    }
    let factor = 3
    let scale: (Int) -> Int = { v in return v * factor }
    for i in 0..20 {
        total = total + scale(i)
    }
    print(total)
    return
}
"#,
    );
}

#[test]
fn a_closure_returning_a_string_agrees_with_the_vm() {
    // wasm's i32 word makes a fabricated integer return for a pointer-shaped
    // result the same hazard it is on LLVM, so a non-scalar result is pinned
    // here too rather than left to the scalar cases.
    assert_parity(
        r#"
@Main
function main() {
    let greet: () -> String = { in return "hello" }
    print(greet())
    return
}
"#,
    );
}

#[test]
fn a_closure_returning_a_struct_agrees_with_the_vm() {
    assert_parity(
        r#"
struct Point {
    let x: Int
    let y: Int
}

function pick(which: Int, a: borrow () -> Point, b: borrow () -> Point) -> Point {
    if which == 0 {
        return a()
    }
    return b()
}

@Main
function main() {
    let one: () -> Point = { in return Point { x: 1, y: 2 } }
    let two: () -> Point = { in return Point { x: 3, y: 4 } }
    print(pick(0, one, two).x)
    print(pick(1, one, two).y)
    return
}
"#,
    );
}

#[test]
fn a_function_type_with_no_literal_anywhere_still_lowers() {
    assert_parity(
        r#"
function apply(f: borrow (Int) -> String) -> String {
    return f(1)
}

@Main
function main() {
    print("ok")
    return
}
"#,
    );
}
