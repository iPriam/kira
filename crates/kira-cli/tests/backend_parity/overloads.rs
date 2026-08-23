//! Parity for overloading: several declarations sharing a name must reach the
//! same one at run time on the vm, llvm, and hybrid backends.
//!
//! This is the execution requirement behind the resolution rule. Every overload
//! is a separate function with a symbol of its own, so a backend that mangled
//! two into one, or a call that reached the wrong one, fails here rather than
//! type-checking cleanly and running the wrong body.

use crate::assert_parity;

/// Free functions sharing a name are told apart by what the call passes, and
/// each runs its own body.
#[test]
fn overloaded_functions_reach_their_own_bodies() {
    let output = assert_parity(
        r#"
function describe(n: Int) -> Int { return n * 10 }
function describe(text: String) -> Int { return text.count * 100 }
function describe(a: Int, b: Int) -> Int { return a + b }
function describe(f: Float) -> Int { return 7 }

@Main function main() {
    print(describe(3))
    print(describe("hi"))
    print(describe(2, 5))
    print(describe(1.5))
    return
}
"#,
    );
    assert_eq!(output, "30\n200\n7\n7\n");
}

/// An argument that already *is* the parameter's type reaches the overload
/// declaring it, rather than the one it would have to be erased into.
#[test]
fn the_closer_overload_wins_at_run_time() {
    let output = assert_parity(
        r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    function area() -> Int { return side * side }
}

function size(s: Any Shape) -> Int { return s.area() }
function size(s: Square) -> Int { return s.area() * 1000 }

@Main function main() {
    print(size(Square(side: 3)))
    let erased: Any Shape = Square(side: 2)
    print(size(move erased))
    return
}
"#,
    );
    assert_eq!(output, "9000\n4\n");
}

/// Overloads compose with parameter defaults: the call that fills every slot
/// itself takes the declaration that needs no default.
#[test]
fn overloads_and_defaults_compose() {
    let output = assert_parity(
        r#"
function tag(n: Int) -> Int { return n }
function tag(n: Int, times: Int = 3) -> Int { return n * times }

@Main function main() {
    print(tag(5))
    print(tag(5, 4))
    return
}
"#,
    );
    assert_eq!(output, "5\n20\n");
}

/// A struct's and a construct-backed declaration's methods overload, and each
/// call runs the body its arguments chose.
#[test]
fn overloaded_methods_run_the_chosen_body() {
    let output = assert_parity(
        r#"
struct Vec2 {
    var x: Int = 0

    function scaled(by: Int) -> Int { return x * by }
    function scaled(by: Int, plus: Int) -> Int { return x * by + plus }
    function scaled(label: String) -> Int { return label.count }
}

construct Widget {
    @Required function total() -> Int
}

construct Leaf(number: Int) extends Widget {
    function total() -> Int { return number }
    function grown(by: Int) -> Int { return number + by }
    function grown(by: Int, times: Int) -> Int { return (number + by) * times }
}

@Main function main() {
    let v = Vec2 { x: 2 }
    print(v.scaled(by: 4))
    print(v.scaled(by: 4, plus: 1))
    print(v.scaled(label: "abc"))
    let leaf = Leaf(number: 3)
    print(leaf.grown(by: 1))
    print(leaf.grown(by: 1, times: 5))
    return
}
"#,
    );
    assert_eq!(output, "8\n9\n3\n4\n20\n");
}

/// A subclass overriding one overload inherits the rest, and both reach the
/// right body on every backend.
#[test]
fn overriding_one_overload_leaves_the_others_running() {
    let output = assert_parity(
        r#"
class Counter {
    var total: Int = 0
    function bump() -> Int { self.total = self.total + 1 return self.total }
    function bump(step: Int) -> Int { self.total = self.total + step return self.total }
}

class Fast extends Counter {
    override function bump(step: Int) -> Int {
        self.total = self.total + step * 100
        return self.total
    }
}

@Main function main() {
    var slow = Counter { }
    print(slow.bump())
    print(slow.bump(step: 10))
    var fast = Fast { }
    print(fast.bump())
    print(fast.bump(step: 3))
    return
}
"#,
    );
    assert_eq!(output, "1\n11\n1\n301\n");
}

/// A `{ … }` with no `in` runs as a closure where the parameter is a function
/// and as children where the callee is a construct-backed declaration, on every
/// backend.
///
/// The parser carries both readings of the same brace; this is the proof that
/// the one analysis chose is the one that runs.
#[test]
fn a_brace_without_in_runs_as_whatever_the_callee_asked_for() {
    let output = assert_parity(
        r#"
function doThing(first: () -> Void, second: () -> Void, third: () -> Void) {
    first()
    second()
    third()
    return
}

function pick(n: Int, ok: () -> Int, other: () -> Int) -> Int {
    if n > 0 { return ok() }
    return other()
}

construct Widget {
    @Required function total() -> Int
}

construct Text(text: String) extends Widget {
    function total() -> Int { return text.count }
}

construct Row(gap: Int = 0) extends Widget {
    @Content let children: [Any Widget]
    function total() -> Int {
        var sum = gap
        for child in children { sum = sum + child.total() }
        return sum
    }
}

@Main function main() {
    // Paren-less, three trailing closures, the last two named.
    doThing {
        print("first")
    } second: {
        print("second")
    } third: {
        print("third")
    }
    // A parenthesized argument, then a closure, then a named closure.
    print(pick(3) { return 1 } other: { return 2 })
    print(pick(0) { return 1 } other: { return 2 })
    // The same brace shape, on a callee that takes children.
    print(Row(gap: 1) { Text(text: "ab") Text(text: "cde") }.total())
    return
}
"#,
    );
    assert_eq!(output, "first\nsecond\nthird\n1\n2\n6\n");
}
