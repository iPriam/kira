//! Parity for classes: inheritance, overrides, and parent-qualified calls.
//!
//! These cases exist because of a divergence the *reference* implementation
//! carries: there, an inherited method calling `self.m()` where a descendant
//! overrides `m` dispatches virtually on vm/hybrid and statically on llvm, and
//! its own corpus steers around the disagreement rather than pinning it.
//!
//! This port removes the disagreement instead of reproducing it. A class gets
//! its own copy of every inherited method, with `self` typed as that class, so
//! there is nothing left to dispatch at run time — which means the shape the
//! reference avoids is exactly the shape worth testing here. `inherited_self_
//! dispatch_reaches_the_override` is that test.

use crate::assert_parity;

#[test]
fn inherited_fields_and_methods_agree() {
    let output = assert_parity(
        r#"
class ClsAccount {
    var balance: Int = 100
    let rate: Int = 2

    function gross() -> Int {
        return self.balance * self.rate
    }
}

class ClsSavings extends ClsAccount {
    override let rate = 5
}

@Main
function main() {
    let a = ClsAccount()
    print(a.gross())
    // The inherited method reads the single `rate` slot, whose default this
    // subclass replaced: an override rebinds storage, it does not add any.
    let s = ClsSavings()
    print(s.gross())
    return
}
"#,
    );
    assert_eq!(output, "200\n500\n");
}

#[test]
fn overriding_a_method_replaces_it() {
    let output = assert_parity(
        r#"
class ClsBase {
    function tier() -> Int {
        return 1
    }
}

class ClsMiddle extends ClsBase {
    override function tier() -> Int {
        return 2
    }
}

class ClsLeaf extends ClsMiddle {
    override function tier() -> Int {
        return 3
    }
}

@Main
function main() {
    print(ClsBase().tier())
    print(ClsMiddle().tier())
    print(ClsLeaf().tier())
    return
}
"#,
    );
    assert_eq!(output, "1\n2\n3\n");
}

#[test]
fn inherited_self_dispatch_reaches_the_override() {
    // The case the reference corpus documents as a vm/llvm divergence and then
    // avoids. Here every backend must agree, because `describe` is compiled
    // once per class with `self` typed as that class.
    let output = assert_parity(
        r#"
class ClsShape {
    function sides() -> Int {
        return 0
    }

    function describe() -> Int {
        return self.sides() * 10
    }
}

class ClsTriangle extends ClsShape {
    override function sides() -> Int {
        return 3
    }
}

class ClsSquare extends ClsShape {
    override function sides() -> Int {
        return 4
    }
}

@Main
function main() {
    print(ClsShape().describe())
    print(ClsTriangle().describe())
    print(ClsSquare().describe())
    return
}
"#,
    );
    assert_eq!(output, "0\n30\n40\n");
}

#[test]
fn parent_qualified_calls_run_the_parent_body_on_this_instance() {
    // `ClsAccount.gross()` is how this language spells "super": the parent's
    // body, the derived instance's fields.
    let output = assert_parity(
        r#"
class ClsAccount {
    var balance: Int = 100
    let rate: Int = 2

    function gross() -> Int {
        return self.balance * self.rate
    }
}

class ClsSavings extends ClsAccount {
    override let rate = 5

    function bonus() -> Int {
        return ClsAccount.gross() + self.balance
    }
}

@Main
function main() {
    print(ClsSavings().bonus())
    return
}
"#,
    );
    assert_eq!(output, "600\n");
}

#[test]
fn a_parent_qualified_call_reaches_a_shadowed_body() {
    // `ClsSquare.scaledArea` is overridden in `ClsCube`, so the parent's body
    // is reachable only by qualifying it — and it must be the parent's, not the
    // override's, or this would not terminate.
    let output = assert_parity(
        r#"
class ClsShape {
    function scaledArea(k: Int) -> Int {
        return 0
    }
}

class ClsSquare extends ClsShape {
    var side: Int = 1

    override function scaledArea(k: Int) -> Int {
        return self.side * self.side * k
    }
}

class ClsCube extends ClsSquare {
    override function scaledArea(k: Int) -> Int {
        return ClsSquare.scaledArea(k) * 6
    }
}

@Main
function main() {
    var c = ClsCube()
    c.side = 3
    print(c.scaledArea(2))
    return
}
"#,
    );
    assert_eq!(output, "108\n");
}

#[test]
fn multiple_inheritance_keeps_both_parents_members() {
    // Two parents declaring `v` is two slots, not one, and each is reachable by
    // qualifying it. A flattening that collapsed them would print 6 or 8.
    let output = assert_parity(
        r#"
class ClsAlpha {
    let v: Int = 3

    function weight() -> Int {
        return 7
    }
}

class ClsBeta {
    let v: Int = 4

    function weight() -> Int {
        return 9
    }
}

class ClsCombo extends ClsAlpha, ClsBeta {
    function blendFields() -> Int {
        return ClsAlpha.v + ClsBeta.v
    }

    function blendWeights() -> Int {
        return ClsAlpha.weight() + ClsBeta.weight()
    }
}

@Main
function main() {
    let c = ClsCombo()
    print(c.blendFields())
    print(c.blendWeights())
    return
}
"#,
    );
    assert_eq!(output, "7\n16\n");
}

#[test]
fn a_class_may_extend_a_struct() {
    let output = assert_parity(
        r#"
struct StrBase {
    let tag: Int = 11

    function label() -> Int {
        return self.tag * 2
    }
}

class ClsDerived extends StrBase {
    override let tag = 21
}

@Main
function main() {
    print(ClsDerived().label())
    return
}
"#,
    );
    assert_eq!(output, "42\n");
}

#[test]
fn a_class_instance_is_a_value_not_a_reference() {
    // A class copies like a struct: mutating the copy must leave the original
    // alone. Nothing in the reference corpus binds a class instance twice, so
    // this pins the answer the flattening implies rather than one it inherits.
    let output = assert_parity(
        r#"
class ClsCounter {
    var value: Int = 1
}

@Main
function main() {
    var first = ClsCounter()
    var second = first
    second.value = 99
    print(first.value)
    print(second.value)
    return
}
"#,
    );
    assert_eq!(output, "1\n99\n");
}

#[test]
fn a_constructor_fills_default_less_fields_positionally() {
    let output = assert_parity(
        r#"
class ClsHolder {
    let values: [Int]
    let scale: Int = 10

    function score() -> Int {
        return self.values[0] * self.scale + self.values.count
    }
}

@Main
function main() {
    let holder = ClsHolder([7, 8, 9])
    print(holder.score())
    return
}
"#,
    );
    assert_eq!(output, "73\n");
}

#[test]
fn classes_work_in_arrays_and_across_calls() {
    let output = assert_parity(
        r#"
class ClsShape {
    let sides: Int = 0

    function baseCost() -> Int {
        return 1
    }
}

class ClsSquare extends ClsShape {
    override let sides = 4
    var side: Int = 1

    function area() -> Int {
        return self.side * self.side
    }
}

function totalCost(square: ClsSquare) -> Int {
    return square.area() + square.sides + square.baseCost()
}

@Main
function main() {
    var squares: [ClsSquare] = []
    var i = 0
    while i < 4 {
        var sq = ClsSquare()
        sq.side = i + 1
        squares.append(move sq)
        i = i + 1
    }
    var total = 0
    var j = 0
    while j < squares.count {
        total = total + totalCost(move squares[j])
        j = j + 1
    }
    print(total)
    return
}
"#,
    );
    assert_eq!(output, "50\n");
}

#[test]
fn a_bare_name_reaches_an_inherited_member() {
    let output = assert_parity(
        r#"
class ClsBase {
    let step: Int = 6

    function unit() -> Int {
        return 7
    }
}

class ClsChild extends ClsBase {
    function total() -> Int {
        // Both written bare: a class body reaches an inherited field and an
        // inherited method without spelling `self`.
        return step + unit()
    }
}

@Main
function main() {
    print(ClsChild().total())
    return
}
"#,
    );
    assert_eq!(output, "13\n");
}
