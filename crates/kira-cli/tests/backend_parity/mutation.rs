//! Parity for mutating `self` in a method.
//!
//! A method may write its receiver — `self.field = x`, `self.xs.append(v)`, or a
//! mutating method on a `self`-rooted place — and the write is observable to the
//! caller after the call. The receiver is passed by value-semantics writeback:
//! the VM writes the callee's final receiver back through the call site's place,
//! and the native backend passes the receiver by pointer so the write lands in
//! the caller's storage directly. These cases prove the two mechanisms agree,
//! byte for byte, with the hybrid bundle — a divergence here is a real bug.

use crate::assert_parity;

/// A struct method that assigns `self.field` is observable after the call, and
/// the same call still yields the method's declared return value.
#[test]
fn a_struct_method_mutating_self_is_observable() {
    let output = assert_parity(
        r#"
struct Counter {
    var n: Int = 0

    function bump(by: Int) -> Int {
        self.n = self.n + by
        return self.n
    }
}

@Main
function main() {
    var c = Counter { n: 10 }
    let r = c.bump(5)
    print(r)
    print(c.n)
    c.bump(100)
    print(c.n)
    return
}
"#,
    );
    // bump(5) returns 15 and leaves n at 15; bump(100) leaves it at 115.
    assert_eq!(output, "15\n15\n115\n");
}

/// A class method that mutates `self` is observable too: a class is a value the
/// same as a struct, and its receiver is written back the same way.
#[test]
fn a_class_method_mutating_self_is_observable() {
    let output = assert_parity(
        r#"
class Account {
    var balance: Int = 100

    function deposit(amount: Int) {
        self.balance = self.balance + amount
    }

    function withdraw(amount: Int) -> Bool {
        if amount > self.balance {
            return false
        }
        self.balance = self.balance - amount
        return true
    }
}

@Main
function main() {
    var a = Account()
    a.deposit(25)
    print(a.balance)
    let ok = a.withdraw(50)
    print(ok)
    print(a.balance)
    let bad = a.withdraw(1000)
    print(bad)
    print(a.balance)
    return
}
"#,
    );
    // 100 +25 -> 125, withdraw 50 ok -> 75, withdraw 1000 refused -> 75.
    assert_eq!(output, "125\ntrue\n75\nfalse\n75\n");
}

/// Binding a struct copies it, so mutating the copy through a method must not
/// reach the original — value semantics survives writeback on every backend.
#[test]
fn mutating_a_copy_does_not_reach_the_original() {
    let output = assert_parity(
        r#"
struct Box {
    var value: Int = 0

    function set(to: Int) {
        self.value = to
    }
}

@Main
function main() {
    var a = Box { value: 1 }
    var b = a
    b.set(99)
    print(a.value)
    print(b.value)
    return
}
"#,
    );
    assert_eq!(output, "1\n99\n");
}

/// Appending through `self` and mutating a nested `self.field` both write the
/// receiver, and a mutating method called on a `self`-rooted place writes that
/// field back.
#[test]
fn appending_and_nested_field_mutation_through_self_agree() {
    let output = assert_parity(
        r#"
struct Bucket {
    var items: [Int] = []

    function add(item: Int) {
        self.items.append(item)
    }
}

struct Holder {
    var bucket: Bucket = Bucket {}
    var total: Int = 0

    function collect(item: Int) {
        self.bucket.add(item)
        self.total = self.total + item
    }
}

@Main
function main() {
    var h = Holder {}
    h.collect(3)
    h.collect(7)
    print(h.bucket.items.count)
    print(h.bucket.items[0])
    print(h.bucket.items[1])
    print(h.total)
    return
}
"#,
    );
    assert_eq!(output, "2\n3\n7\n10\n");
}

/// A `Bool`-returning mutator inside a short-circuiting `&&` mutates only when
/// evaluated, because the writeback is a side effect of evaluating the call
/// expression rather than a hoisted statement. The corpus relies on this shape.
#[test]
fn a_mutator_in_a_short_circuit_only_mutates_when_evaluated() {
    let output = assert_parity(
        r#"
struct Slot {
    var used: Bool = false

    function take() -> Bool {
        if self.used {
            return false
        }
        self.used = true
        return true
    }
}

struct Pool {
    var a: Slot = Slot {}
    var b: Slot = Slot {}

    function grab() -> Int {
        var count = 0
        if self.a.take() && self.b.take() {
            count = count + 1
        }
        // `a.take()` is now false, so `b.take()` must not run a second time.
        if self.a.take() && self.b.take() {
            count = count + 1
        }
        return count
    }
}

@Main
function main() {
    var p = Pool {}
    print(p.grab())
    print(p.a.used)
    print(p.b.used)
    return
}
"#,
    );
    // The first `if` claims both slots; the second short-circuits on `a`, so `b`
    // is claimed exactly once and the count is 1.
    assert_eq!(output, "1\ntrue\ntrue\n");
}
