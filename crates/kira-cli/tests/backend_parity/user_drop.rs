//! Parity for the `Drop` trait: a user body run where each engine already
//! releases the value.
//!
//! The two engines reach the body by different roads — the native backend calls
//! it at the head of the type's release leaf, the VM parks the object when its
//! last holder goes and dispatches the body between instructions — so *when* it
//! runs is exactly what these prove. Ordering is the whole content of the
//! output: every line is one body, and a body that ran at a different moment on
//! one engine fails here.

use crate::{assert_parity, assert_parity_with_heap_balance};

/// A scope, a move into a callee, and a borrow: three moments, one order.
#[test]
fn a_drop_body_runs_at_the_same_moment_on_both_engines() {
    let output = assert_parity(
        r#"
struct DrpTrace: Drop {
    let tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

function scoped(tag: Int) -> Int {
    let held = DrpTrace(tag: tag)
    return tag * 2
}

function consume(held: move DrpTrace) -> Int {
    return held.tag + 100
}

function read(held: borrow DrpTrace) -> Int {
    return held.tag * 10
}

@Main
function main() {
    print(scoped(1))
    let moved = DrpTrace(tag: 2)
    print(consume(move moved))
    let kept = DrpTrace(tag: 3)
    print(read(kept))
    print(999)
    return
}
"#,
    );
    // `1` before `2`: the scope's value dies inside `scoped`, before its answer
    // is printed. `2` before `102`: the moved value dies in the callee that
    // took it, not at the binding it left. `3` last: a borrow releases nothing,
    // so `kept` outlives every line of `main`.
    assert_eq!(output, "1\n2\n2\n102\n30\n999\n3\n");
}

/// The body runs before the members are released, so it can still read them.
#[test]
fn a_drop_body_reads_the_members_the_release_frees_after_it() {
    let output = assert_parity(
        r#"
struct DrpNamed: Drop {
    let name: String
    function drop(borrow mut self) { print(self.name) return }
}

@Main
function main() {
    let held = DrpNamed(name: "clo" + "sing")
    print("open")
    return
}
"#,
    );
    assert_eq!(output, "open\nclosing\n");
}

/// A container releases every `Drop` value it holds, in reverse field order:
/// drops run in reverse completion order, and a struct's fields complete in
/// declaration order.
#[test]
fn a_container_runs_the_body_of_every_drop_it_holds() {
    let output = assert_parity(
        r#"
struct DrpTrace: Drop {
    let tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

struct DrpPair {
    let first: DrpTrace
    let second: DrpTrace
}

@Main
function main() {
    let pair = DrpPair(first: DrpTrace(tag: 1), second: DrpTrace(tag: 2))
    print(pair.first.tag + pair.second.tag)
    return
}
"#,
    );
    assert_eq!(output, "3\n2\n1\n");
}

/// Churning a heap-owning `Drop` type leaves the native heap balanced: a body
/// that ran instead of the release, or a release that skipped the body, shows
/// up as a live count here.
#[test]
fn churning_a_drop_type_leaves_the_native_heap_balanced() {
    let output = assert_parity_with_heap_balance(
        r#"
struct DrpResource: Drop {
    let label: String
    let tag: Int
    function drop(borrow mut self) { return }
}

@Main
function main() {
    var total = 0
    var index = 0
    while index < 200 {
        let held = DrpResource(label: "res" + "ource", tag: index)
        total = total + held.tag
        index = index + 1
    }
    print(total)
    return
}
"#,
    );
    assert_eq!(output, "19900\n");
}

/// A recovered callback-state value is a view, not a second owner. Reading or
/// writing through that view must not run its user `Drop` body; the body belongs
/// to the state itself and runs exactly once when the final userdata owner is
/// released.
#[test]
fn native_state_recover_is_an_alias_and_drops_exactly_once() {
    let output = assert_parity_with_heap_balance(
        r#"
struct NativeDrp: Drop {
    var label: String
    var tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

@Main
function main() {
    let token = nativeUserData(nativeState(NativeDrp(label: "state", tag: 1)))
    var view = nativeRecover<NativeDrp>(token)
    print(view.tag)
    view.tag = 2
    print(view.tag)
    nativeUserDataRelease(token)
    return
}
"#,
    );
    assert_eq!(output, "1\n2\n2\n");
}

/// Hybrid native functions use backend-neutral value-tree state rather than the
/// whole-program LLVM box. A field read through the recovered view may therefore
/// materialize a temporary snapshot, whose field storage must be cleaned up
/// without running the root `Drop` body as though the snapshot were an owner.
#[test]
fn native_hybrid_recover_snapshot_does_not_run_drop_early() {
    let output = assert_parity_with_heap_balance(
        r#"
struct NativeHybridDrp: Drop {
    var label: String
    var tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

@Native
function nativeWork() {
    let token = nativeUserData(nativeState(NativeHybridDrp(label: "state", tag: 4)))
    var view = nativeRecover<NativeHybridDrp>(token)
    print(view.tag)
    view.tag = 5
    nativeUserDataRelease(token)
    return
}

@Main
function main() {
    nativeWork()
    return
}
"#,
    );
    assert_eq!(output, "4\n5\n");
}

/// A state handle is affine, so an owner that is not explicitly released still
/// destroys its Drop-bearing value when the handle leaves scope.
#[test]
fn drop_native_state_is_destroyed_by_automatic_scope_release() {
    let output = assert_parity_with_heap_balance(
        r#"
struct ScopedNativeDrp: Drop {
    var label: String
    var tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

@Native
function nativeWork() {
    let state = nativeState(ScopedNativeDrp(label: "scope", tag: 7))
    return
}

@Main
function main() {
    nativeWork()
    return
}
"#,
    );
    assert_eq!(output, "7\n");
}

#[test]
fn nested_drop_in_callback_state_releases_displaced_and_final_values() {
    let output = assert_parity_with_heap_balance(
        r#"
struct NestedDropLeaf: Drop {
    var tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

struct NestedDropBox {
    var leaf: NestedDropLeaf
}

@Native
function nativeWork() {
    let state = nativeState(NestedDropBox { leaf: NestedDropLeaf(tag: 1) })
    var view = nativeRecover<NestedDropBox>(state)
    print(view.leaf.tag + 10)
    view.leaf = NestedDropLeaf(tag: 2)
    print(view.leaf.tag + 20)
    return
}

@Main
function main() {
    nativeWork()
    return
}
"#,
    );
    assert_eq!(output, "11\n1\n22\n2\n");
}

#[test]
fn nested_native_state_owner_releases_through_the_active_engine() {
    let output = assert_parity_with_heap_balance(
        r#"
struct NestedStatePayload: Drop {
    var tag: Int
    function drop(borrow mut self) { print(self.tag) return }
}

struct NestedStateBox {
    var child: NativeState<NestedStatePayload>
}

@Native
function nativeWork() {
    let child = nativeState(NestedStatePayload(tag: 1))
    let outer = nativeState(NestedStateBox { child: move child })
    var view = nativeRecover<NestedStateBox>(outer)
    var childView = nativeRecover<NestedStatePayload>(view.child)
    print(childView.tag)
    view.child = nativeState(NestedStatePayload(tag: 2))
    print(9)
    return
}

@Main
function main() {
    nativeWork()
    return
}
"#,
    );
    assert_eq!(output, "1\n1\n9\n2\n");
}
