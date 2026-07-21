//! Tests for mutating `self` in a method.
//!
//! A method may write its receiver when its body assigns through `self`, appends
//! through it, or calls another mutating method on it. Writing `self.field` is
//! then accepted, the field's own `var`/`let` rule still applies, and a mutating
//! call at the call site demands a mutable receiver — a `let` binding is refused
//! with the same `KSEM021` an assignment to it would earn.

use super::codes;

/// A struct method that assigns a `var` field of `self` type-checks.
#[test]
fn a_struct_method_may_assign_a_var_field_of_self() {
    assert!(
        codes(
            r#"
struct Counter {
    var n: Int = 0
    function bump() { self.n = self.n + 1 }
}
@Main
function main() { var c = Counter { n: 0 } c.bump() print(c.n) return }
"#,
        )
        .is_empty()
    );
}

/// Assigning a `let` field of `self` is refused: a mutating receiver does not
/// make an immutable field writable.
#[test]
fn a_struct_method_cannot_assign_a_let_field_of_self() {
    assert_eq!(
        codes(
            r#"
struct Counter {
    let n: Int = 0
    function bump() { self.n = self.n + 1 }
}
@Main
function main() { return }
"#,
        ),
        vec!["KSEM024"],
    );
}

/// Calling a mutating method through an immutable binding is refused, and the
/// receiver-isn't-mutable case keeps `KSEM021`.
#[test]
fn calling_a_mutator_through_an_immutable_binding_is_refused() {
    assert_eq!(
        codes(
            r#"
struct Counter {
    var n: Int = 0
    function bump() { self.n = self.n + 1 }
}
@Main
function main() { let c = Counter { n: 0 } c.bump() return }
"#,
        ),
        vec!["KSEM021"],
    );
}

/// Calling a mutating method on a temporary value is refused: there is no
/// storage for the mutation to be written back to.
#[test]
fn calling_a_mutator_on_a_temporary_is_refused() {
    assert_eq!(
        codes(
            r#"
struct Counter {
    var n: Int = 0
    function bump() { self.n = self.n + 1 }
}
@Main
function main() { Counter { n: 0 }.bump() return }
"#,
        ),
        vec!["KSEM211"],
    );
}

/// A class method that mutates `self` type-checks, exactly as a struct method
/// does — a class is a value with the same writeback.
#[test]
fn a_class_method_may_mutate_self() {
    assert!(
        codes(
            r#"
class Account {
    var balance: Int = 0
    function deposit(amount: Int) { self.balance = self.balance + amount }
}
@Main
function main() { var a = Account() a.deposit(5) print(a.balance) return }
"#,
        )
        .is_empty()
    );
}

/// A non-mutating method still leaves its receiver immutable, so it is callable
/// on a `let` binding — the read-only receiver is not disturbed.
#[test]
fn a_non_mutating_method_is_callable_on_an_immutable_binding() {
    assert!(
        codes(
            r#"
struct Counter {
    var n: Int = 0
    function doubled() -> Int { return self.n * 2 }
}
@Main
function main() { let c = Counter { n: 21 } print(c.doubled()) return }
"#,
        )
        .is_empty()
    );
}

/// Mutation reaches through a `self`-rooted place: a method that only mutates
/// `self` by calling a mutating method on one of its fields is itself mutating,
/// so it too demands a mutable receiver at its call sites.
#[test]
fn a_transitively_mutating_method_demands_a_mutable_receiver() {
    let source = r#"
struct Inner {
    var n: Int = 0
    function bump() { self.n = self.n + 1 }
}
struct Outer {
    var inner: Inner = Inner { n: 0 }
    function run() { self.inner.bump() }
}
@Main
function main() { let o = Outer { inner: Inner { n: 0 } } o.run() return }
"#;
    // `run` mutates `self.inner`, so it is mutating; calling it on `let o` is
    // refused with the immutable-binding code.
    assert_eq!(codes(source), vec!["KSEM021"]);
}
