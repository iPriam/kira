use super::*;

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
    // A non-owning `RawPtr` from the callback-state box: an opaque host handle,
    // exactly what a real event loop captures alongside its handler. The affine
    // owner stays in `owner`; a closure may only capture the trivially-copyable
    // borrow word, not the owner itself.
    let owner = nativeUserData(boxed)
    let handle = nativeUserDataBorrow(owner)
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
    nativeUserDataRelease(owner)
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
