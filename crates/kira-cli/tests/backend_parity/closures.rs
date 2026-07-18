//! Parity for closures: the VM, the LLVM/native backend, and the hybrid bundle
//! must agree.
//!
//! A closure reaches the IR as a struct plus two ordinary calls — the desugar
//! happens in semantics, so no backend has a closure-specific path at all.
//! These cases are what proves that: had function types, closure literals, or
//! calls through a closure value needed a node of their own, each backend would
//! have had to lower it, and none did.

use crate::assert_parity;

#[test]
fn a_closure_captures_a_let_by_value() {
    let output = assert_parity(
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
    assert_eq!(output, "101\n105\n");
}

#[test]
fn a_closure_is_returned_from_a_function() {
    // The return-type-via-colon spelling: `->` would be ambiguous with the
    // function type's own arrow, so a function type result is written after
    // `:`.
    let output = assert_parity(
        r#"
function makeAdder(step: Int): (Int) -> Int {
    return { value in
        return value + step
    }
}

@Main
function main() {
    let add3 = makeAdder(3)
    let add9 = makeAdder(9)
    print(add3(10))
    print(add9(10))
    return
}
"#,
    );
    // Two values of one function type, built by one literal, carrying different
    // captures: the tag alone would give the same answer twice.
    assert_eq!(output, "13\n19\n");
}

#[test]
fn a_trailing_closure_is_the_last_argument() {
    let output = assert_parity(
        r#"
function register(handler: (Int) -> Int) -> Int {
    return handler(2)
}

function registerWith(seed: Int, handler: (Int) -> Int) -> Int {
    return handler(seed)
}

@Main
function main() {
    let base = 30
    print(register { value in
        return base + value
    })
    print(registerWith(5) { value in
        return base * value
    })
    return
}
"#,
    );
    assert_eq!(output, "32\n150\n");
}

#[test]
fn a_zero_parameter_closure_writes_a_bare_in() {
    let output = assert_parity(
        r#"
function run(handler: () -> Int) -> Int {
    return handler()
}

@Main
function main() {
    let message = 40
    print(run { in
        return message
    })
    return
}
"#,
    );
    assert_eq!(output, "40\n");
}

#[test]
fn a_closure_takes_several_parameters() {
    let output = assert_parity(
        r#"
function combine(f: (Int, Int) -> Int) -> Int {
    return f(6, 7)
}

@Main
function main() {
    print(combine { a, b in
        return a * b
    })
    return
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn a_parameter_shadows_a_capture_of_the_same_name() {
    let output = assert_parity(
        r#"
function register(handler: (Int) -> Int) -> Int {
    return handler(1)
}

@Main
function main() {
    let value = 123
    print(register { value in
        return value
    })
    print(value)
    return
}
"#,
    );
    // The parameter wins inside, and the outer binding is untouched outside.
    assert_eq!(output, "1\n123\n");
}

#[test]
fn a_capture_is_readable_before_an_inner_binding_shadows_it() {
    let output = assert_parity(
        r#"
function run(handler: () -> Int) -> Int {
    return handler()
}

@Main
function main() {
    let shadowed = 20
    print(run { in
        let before = shadowed
        let shadowed = 21
        return before + shadowed
    })
    return
}
"#,
    );
    // `shadowed` is free at its first use and bound at its second, so a
    // whole-body name scan would get this wrong in both directions.
    assert_eq!(output, "41\n");
}

#[test]
fn a_nested_closure_captures_through_the_one_between() {
    let output = assert_parity(
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
    // `base` lives two frames out, so it is captured into the middle closure
    // and then out of it — not reached past.
    assert_eq!(output, "32\n");
}

#[test]
fn two_literals_of_one_type_keep_their_own_captures() {
    let output = assert_parity(
        r#"
function invoke(f: borrow (Int) -> Int, x: Int) -> Int {
    return f(x)
}

@Main
function main() {
    let a = 10
    let first: (Int) -> Int = { v in return v + a }
    let b = 100
    let second: (Int) -> Int = { v in return v + b }
    print(invoke(first, 1) + invoke(second, 1))
    return
}
"#,
    );
    // Sharing one representation slot between the two would give 11 + 11 = 22.
    assert_eq!(output, "112\n");
}

#[test]
fn a_closure_is_stored_in_a_field_and_called_through_it() {
    let output = assert_parity(
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
    assert_eq!(output, "85\n");
}

#[test]
fn a_trailing_closure_on_a_method_receives_a_constructed_value() {
    let output = assert_parity(
        r#"
class Frame {
    let value: Int

    function draw() -> Int {
        return self.value
    }
}

class Graphics {
    function run(handler: (Frame) -> Int) -> Int {
        return handler(Frame(1))
    }

    function runWithConfig(config: Int, handler: (Frame) -> Int) -> Int {
        return handler(Frame(config))
    }
}

@Main
function main() {
    let graphics = Graphics()
    print(graphics.run { frame in return frame.draw() })
    print(graphics.runWithConfig(7) { frame in return frame.draw() })
    return
}
"#,
    );
    assert_eq!(output, "1\n7\n");
}

#[test]
fn a_void_closure_runs_for_effect() {
    let output = assert_parity(
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
    assert_eq!(output, "10\n20\n");
}

#[test]
fn a_closure_is_called_inside_a_loop() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let factor = 3
    let scale: (Int) -> Int = { v in return v * factor }
    var total = 0
    for i in 0..4 {
        total = total + scale(i)
    }
    print(total)
    return
}
"#,
    );
    // 3 * (0 + 1 + 2 + 3)
    assert_eq!(output, "18\n");
}
