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

#[test]
fn a_closure_returning_a_string_agrees_on_every_backend() {
    // A non-scalar result exercises the dispatcher's return path.
    let output = assert_parity(
        r#"
@Main
function main() {
    let greet: () -> String = { in return "hello" }
    print(greet())
    return
}
"#,
    );
    assert_eq!(output, "hello\n");
}

#[test]
fn a_closure_returning_a_struct_agrees_on_every_backend() {
    let output = assert_parity(
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
    assert_eq!(output, "1\n4\n");
}

#[test]
fn a_function_type_with_no_literal_anywhere_still_builds() {
    // `apply` is never called and no closure of its parameter's type exists, so
    // the dispatcher this mints has zero branches. It is unreachable, but every
    // backend still type-checks it, so its terminator must be well typed.
    let output = assert_parity(
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
    assert_eq!(output, "ok\n");
}

#[test]
fn a_closure_returning_an_array_agrees_on_every_backend() {
    let output = assert_parity(
        r#"
@Main
function main() {
    let names: () -> [String] = { in return ["a", "b", "c"] }
    print(names().count)
    return
}
"#,
    );
    assert_eq!(output, "3\n");
}

/// Calling through a function type whose parameter is `borrow` runs the same on
/// every backend, and leaves the caller's value alone.
///
/// The mode is a static check with no lowering — the value crosses by copy
/// whatever it says — so this case exists to prove exactly that: the answer is
/// identical to the owned spelling, and the caller can still read its value
/// afterwards.
#[test]
fn calling_through_a_borrow_function_type_agrees() {
    let output = assert_parity(
        r#"
struct Event {
    var code: Int
    var label: String
}

function intText(n: Int) -> String {
    if n == 1 {
        return "one"
    }
    return "many"
}

function describe(event: borrow Event) {
    print(event.label + "/" + intText(event.code))
    return
}

@Main
function main() {
    let onEvent: (borrow Event) -> Void = describe
    let e = Event { code: 1, label: "down" }
    onEvent(e)
    onEvent(e)
    print(e.label)
    return
}
"#,
    );
    assert_eq!(output, "down/one\ndown/one\ndown\n");
}

/// Calling through a function type whose parameter is `borrow mut` writes back
/// into the caller's binding on every backend.
///
/// A mutable borrow is the one mode that *is* observable at run time, so unlike
/// `borrow` this is not a static check with no lowering: the dispatcher takes
/// the slot by reference, forwards it, and carries what the arm wrote back out
/// to its own caller. If any backend dropped a link in that chain it would print
/// the unchanged value, so the numbers here are the proof the chain holds.
#[test]
fn calling_through_a_borrow_mut_function_type_agrees() {
    let output = assert_parity(
        r#"
struct Frame {
    var n: Int
    var label: String
}

function bump(frame: borrow mut Frame) {
    frame.n = frame.n + 1
    frame.label = frame.label + "!"
    return
}

@Main
function main() {
    let onFrame: (borrow mut Frame) -> Void = bump
    var f = Frame { n: 1, label: "a" }
    onFrame(f)
    onFrame(f)
    print(f.n)
    print(f.label)
    return
}
"#,
    );
    assert_eq!(output, "3\na!!\n");
}

/// A closure *literal* of a `borrow mut` function type writes back too, and two
/// literals of one type dispatch to the arm their tag names.
#[test]
fn a_borrow_mut_closure_literal_writes_back_and_agrees() {
    let output = assert_parity(
        r#"
struct Frame {
    var n: Int
}

function apply(f: borrow (borrow mut Frame) -> Void, target: borrow mut Frame) {
    f(target)
    return
}

@Main
function main() {
    let double: (borrow mut Frame) -> Void = { g in g.n = g.n * 2 return }
    let inc: (borrow mut Frame) -> Void = { g in g.n = g.n + 3 return }
    var f = Frame { n: 5 }
    double(f)
    print(f.n)
    inc(f)
    print(f.n)
    apply(double, f)
    print(f.n)
    return
}
"#,
    );
    // 5 *2 -> 10, +3 -> 13, *2 -> 26.
    assert_eq!(output, "10\n13\n26\n");
}

/// A `borrow mut` argument reaching a function value through a *nested* place
/// lands back in that same field, not in a copy of the whole binding.
#[test]
fn a_nested_place_written_through_a_function_value_agrees() {
    let output = assert_parity(
        r#"
struct Inner {
    var n: Int
}

struct Outer {
    var left: Inner
    var right: Inner
}

function bump(inner: borrow mut Inner) {
    inner.n = inner.n + 10
    return
}

@Main
function main() {
    let onInner: (borrow mut Inner) -> Void = bump
    var o = Outer { left: Inner { n: 1 }, right: Inner { n: 2 } }
    onInner(o.left)
    print(o.left.n)
    print(o.right.n)
    return
}
"#,
    );
    assert_eq!(output, "11\n2\n");
}

/// A binding that declares `borrow` on its annotation runs identically to one
/// that does not, on every backend.
///
/// The prefix is a static statement about how the binding takes its initializer
/// and is only accepted where owned and borrowed coincide — so if any backend
/// had started treating the binding differently, these two would disagree.
#[test]
fn a_borrow_prefix_on_a_binding_annotation_agrees() {
    let output = assert_parity(
        r#"
struct Frame {
    var n: Int
}

function bump(frame: borrow mut Frame) {
    frame.n = frame.n + 1
    return
}

function twice(f: borrow (borrow mut Frame) -> Void, target: borrow mut Frame) {
    f(target)
    f(target)
    return
}

@Main
function main() {
    let borrowed: borrow (borrow mut Frame) -> Void = bump
    let owned: (borrow mut Frame) -> Void = bump
    var f = Frame { n: 0 }
    borrowed(f)
    owned(f)
    twice(borrowed, f)
    twice(owned, f)
    print(f.n)
    return
}
"#,
    );
    assert_eq!(output, "6\n");
}

/// A `RawPtr` and a function value of another type are both capturable: each
/// copies words and owns nothing, so a closure may carry a host handle and a
/// callback.
///
/// This is what an inline event loop needs — the handler and the opaque host
/// pointer both cross into the closure — and it runs the same on every backend.
/// A capture of the closure's own function type needs an indirection to have a
/// representation at all; that case is
/// [`a_closure_captures_a_function_value_of_its_own_type`].
#[test]
fn a_closure_captures_a_raw_pointer_and_a_function_value() {
    let output = assert_parity(
        r#"
struct Frame {
    var n: Int
}

function bump(frame: borrow mut Frame) {
    frame.n = frame.n + 1
    return
}

struct Host {
    var seed: Int
}

@Main
function main() {
    let boxed = nativeState(Host { seed: 5 })
    // A `RawPtr` from the callback-state box: an opaque host handle, exactly
    // what a real event loop captures alongside its handler.
    let handle = nativeUserData(boxed)
    let step: (Int) -> Int = { v in return v + 1 }

    let apply: (borrow mut Frame) -> Void = { f in
        bump(f)
        f.n = step(f.n)
        var host = nativeRecover<Host>(handle)
        f.n = f.n + host.seed
        return
    }

    var f = Frame { n: 1 }
    apply(f)
    print(f.n)
    nativeStateFree(boxed)
    return
}
"#,
    );
    // 1 bumped to 2, stepped to 3, plus the captured host's seed of 5.
    assert_eq!(output, "8\n");
}

/// A closure captures a function value of the closure's own type, on every
/// backend.
///
/// The capture becomes a field of the closure's representation struct, so
/// storing it inline would make that struct contain itself — a value of no
/// size. It travels behind a one-element array instead: a heap handle, so the
/// struct is a fixed size again, and copying an array copies its element, so the
/// captured function value behaves exactly as an inline field would have.
///
/// The default parameter is what makes both arms observable: the first call
/// captures a function that does nothing, the second one that adds ten, and the
/// closure's own `+ 1` runs either way.
#[test]
fn a_closure_captures_a_function_value_of_its_own_type() {
    let output = assert_parity(
        r#"
struct Frame {
    var value: Int = 0
}

function noopFrame(frame: borrow mut Frame) -> Void {
    return
}

function bump(frame: borrow mut Frame) -> Void {
    frame.value = frame.value + 10
    return
}

function onFrame(handler: borrow (borrow mut Frame) -> Void) -> Void {
    var f = Frame { value: 1 }
    handler(f)
    print(f.value)
    return
}

function runApp(engineFrame: borrow (borrow mut Frame) -> Void = noopFrame) -> Void {
    onFrame({ frame in
        let hostFrame: borrow (borrow mut Frame) -> Void = engineFrame
        hostFrame(frame)
        frame.value = frame.value + 1
        return
    })
    return
}

@Main
function main() {
    runApp()
    runApp(bump)
    return
}
"#,
    );
    assert_eq!(output, "2\n12\n");
}
