//! Parity for classes: the VM and both wasm word sizes must agree.
//!
//! A class reaches this crate as a struct — flattening happens in semantics, so
//! the wasm lowering has no class-specific path at all. These cases are what
//! proves that: if any of inheritance, overriding, or parent qualification had
//! needed a node of its own, it would have had to be lowered here, and it was
//! not.

use crate::assert_parity;

#[test]
fn inherited_fields_and_methods_agree() {
    assert_parity(
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
    print(ClsAccount().gross())
    print(ClsSavings().gross())
    return
}
"#,
    );
}

#[test]
fn overriding_a_method_replaces_it() {
    assert_parity(
        r#"
class ClsBase {
    function tier() -> Int {
        return 1
    }
}

class ClsLeaf extends ClsBase {
    override function tier() -> Int {
        return 2
    }
}

@Main
function main() {
    print(ClsBase().tier())
    print(ClsLeaf().tier())
    return
}
"#,
    );
}

#[test]
fn inherited_self_dispatch_reaches_the_override() {
    assert_parity(
        r#"
class ClsShape {
    function sides() -> Int {
        return 0
    }

    function describe() -> Int {
        return self.sides() * 10
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
    print(ClsSquare().describe())
    return
}
"#,
    );
}

#[test]
fn parent_qualified_calls_agree() {
    assert_parity(
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
}

#[test]
fn multiple_inheritance_agrees() {
    assert_parity(
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
    function blend() -> Int {
        return ClsAlpha.v + ClsBeta.v + ClsAlpha.weight() + ClsBeta.weight()
    }
}

@Main
function main() {
    print(ClsCombo().blend())
    return
}
"#,
    );
}

#[test]
fn a_class_copies_like_a_value() {
    assert_parity(
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
}

#[test]
fn a_constructor_fills_default_less_fields() {
    assert_parity(
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
    print(ClsHolder([7, 8, 9]).score())
    return
}
"#,
    );
}
