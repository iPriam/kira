//! Parity for the ownership modes.
//!
//! Ownership is enforced entirely in the analyzer: for the current type
//! lattice a `move` and a `borrow` are both observationally identical to the
//! deep copy the runtime already performs, so no backend was taught anything.
//!
//! That claim is exactly what these cases test. If `move` or `borrow` had
//! quietly changed how a value reaches a callee on one backend — an alias
//! where another copies, a drop one engine skips — these programs would print
//! different answers on that backend. They print the same answer on all three,
//! which is the evidence that the modes are a static check rather than a
//! silent lowering difference.

use crate::assert_parity;

/// A moved struct reaches its callee intact on every backend, and the pipeline
/// of moves the oracle's `StxoMovePipeline` describes computes the same total.
#[test]
fn moving_a_struct_through_a_pipeline_agrees() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function consume(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

function pipeAdd(v: Vec3, n: Int) -> Vec3 {
    return Vec3 { x: v.x + n, y: v.y + n, z: v.z + n }
}

function pipeMul(v: Vec3, k: Int) -> Vec3 {
    return Vec3 { x: v.x * k, y: v.y * k, z: v.z * k }
}

@Main
function main() {
    let v = Vec3 { x: 2, y: 3, z: 4 }
    let w = pipeAdd(move v, 10)
    let x = pipeMul(move w, 2)
    print(consume(move x))
    return
}
"#,
    );
    // {2,3,4} +10 -> {12,13,14}, *2 -> {24,26,28}, sum 78.
    assert_eq!(output, "78\n");
}

/// A `borrow` parameter leaves the caller's value usable, and reads through it
/// see the same value the caller does.
#[test]
fn borrowing_a_struct_leaves_it_usable_and_agrees() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function vecSum(v: borrow Vec3) -> Int {
    return v.x + v.y + v.z
}

@Main
function main() {
    let v = Vec3 { x: 5, y: 6, z: 7 }
    let s1 = vecSum(v)
    let s2 = vecSum(v)
    print(s1 + s2)
    // The borrow consumed nothing, so the original still reads.
    print(v.z)
    return
}
"#,
    );
    assert_eq!(output, "36\n7\n");
}

/// Binding a struct copies it: mutating the copy must not reach the original
/// on any backend. This is the oracle's `StxoCopyIndepX` / `StxoCopyBothValues`
/// shape, and it is the rule arrays will invert — pinning it here means that
/// inversion has to be deliberate.
#[test]
fn binding_a_struct_copies_it_on_every_backend() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

@Main
function main() {
    let v = Vec3 { x: 5, y: 6, z: 7 }
    var w = v
    w.x = 100
    print(v.x)
    print(w.x + v.x)
    return
}
"#,
    );
    assert_eq!(output, "5\n105\n");
}

/// A copied struct can still be moved onward while the original stays live —
/// the oracle's `StxoCopyThenMoveConsumer`.
#[test]
fn a_copy_can_be_moved_while_the_original_survives() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function consume(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

@Main
function main() {
    let v = Vec3 { x: 3, y: 4, z: 5 }
    let w = v
    print(consume(move w) + v.x)
    return
}
"#,
    );
    // consume({3,4,5}) = 12, plus v.x = 3.
    assert_eq!(output, "15\n");
}

/// `copy` on a trivially-copyable value is accepted and changes nothing.
#[test]
fn copying_a_scalar_agrees() {
    let output = assert_parity(
        r#"
function twice(n: copy Int) -> Int {
    return n + n
}

@Main
function main() {
    let n = 21
    print(twice(copy n))
    // `copy` consumed nothing.
    print(n)
    return
}
"#,
    );
    assert_eq!(output, "42\n21\n");
}

/// A moved `String` reaches its callee whole. Strings own heap bytes, so a
/// backend that got the transfer wrong would print garbage or leak rather than
/// disagree quietly.
#[test]
fn moving_a_string_agrees() {
    let output = assert_parity(
        r#"
function shout(s: String) -> String {
    return s + "!"
}

function width(s: borrow String) -> Bool {
    return s == "kira"
}

@Main
function main() {
    let name = "kira"
    print(width(name))
    print(shout(move name))
    return
}
"#,
    );
    assert_eq!(output, "true\nkira!\n");
}

/// Moves inside a loop allocate and release once per iteration; the leak
/// counter and the arithmetic must agree across backends. The oracle's
/// `StxoMoveVecLoop`.
#[test]
fn moving_in_a_loop_agrees() {
    let output = assert_parity(
        r#"
struct Vec3 {
    var x: Int
    var y: Int
    var z: Int
}

function consume(v: Vec3) -> Int {
    return v.x + v.y + v.z
}

@Main
function main() {
    var acc = 0
    var i = 0
    while i < 10 {
        let v = Vec3 { x: i, y: i, z: i }
        acc = acc + consume(move v)
        i = i + 1
    }
    print(acc)
    return
}
"#,
    );
    // 3 * (0+1+...+9) = 135.
    assert_eq!(output, "135\n");
}

/// A method's receiver borrows, so a method call consumes nothing and repeats
/// identically on every backend.
#[test]
fn a_method_receiver_borrows_on_every_backend() {
    let output = assert_parity(
        r#"
struct Counter {
    var n: Int

    function doubled() -> Int {
        return n * 2
    }
}

@Main
function main() {
    let c = Counter { n: 21 }
    print(c.doubled())
    print(c.doubled())
    print(c.n)
    return
}
"#,
    );
    assert_eq!(output, "42\n42\n21\n");
}

/// A binding reassigned from a move of itself carries the value forward, and
/// every backend sees the same one.
///
/// The analyzer accepts this because an assignment reinitializes the binding.
/// If any backend had aliased the moved value instead of copying it, the loop
/// would read a stale or freed one and the totals would diverge.
#[test]
fn threading_a_value_through_a_reassigned_binding_agrees() {
    let output = assert_parity(
        r#"
struct Tree {
    var depth: Int
    var label: String
}

function grow(tree: Tree, by: Int) -> Tree {
    return Tree { depth: tree.depth + by, label: tree.label }
}

function describe(tree: borrow Tree) -> Int {
    return tree.depth
}

@Main
function main() {
    var tree = Tree { depth: 1, label: "root" }
    tree = grow(move tree, 2)
    print(describe(tree))
    var i = 0
    while i < 4 {
        tree = grow(move tree, i)
        i = i + 1
    }
    print(describe(tree))
    print(tree.label)
    return
}
"#,
    );
    // 1 + 2 = 3, then + (0+1+2+3) = 9.
    assert_eq!(output, "3\n9\nroot\n");
}

/// A `borrow mut` parameter writes through the caller's binding, and every
/// backend sees the same write.
///
/// The VM copies the argument into the callee's slot and moves it back on
/// return; native passes a pointer into the caller's storage and the callee
/// writes through it. Two mechanisms, one observable answer — which is the
/// whole claim this case exists to check.
#[test]
fn a_mutable_borrow_writes_through_on_every_backend() {
    let output = assert_parity(
        r#"
struct Counter {
    var n: Int
    var label: String
}

function bump(c: borrow mut Counter, by: Int) {
    c.n = c.n + by
    return
}

function rename(c: borrow mut Counter, to: String) -> Int {
    c.label = to
    return c.n
}

@Main
function main() {
    var counter = Counter { n: 1, label: "start" }
    bump(counter, 4)
    bump(counter, 5)
    print(counter.n)
    print(rename(counter, "done"))
    print(counter.label)
    return
}
"#,
    );
    assert_eq!(output, "10\n10\ndone\n");
}

/// Several `borrow mut` parameters in one call each land in their own place.
///
/// This is the shape `CallMut` cannot encode — its writeback target is fixed at
/// callee slot 0 — so it is the case that exercises `CallWriteback` on the VM
/// and two pointer parameters on native.
#[test]
fn several_mutable_borrows_in_one_call_agree() {
    let output = assert_parity(
        r#"
struct Bucket {
    var total: Int
}

function move(from: borrow mut Bucket, to: borrow mut Bucket, amount: Int) {
    from.total = from.total - amount
    to.total = to.total + amount
    return
}

function drain(left: borrow mut [Int], right: borrow mut [Int]) {
    left.append(7)
    right.append(8)
    right.append(9)
    return
}

@Main
function main() {
    var a = Bucket { total: 100 }
    var b = Bucket { total: 5 }
    move(a, b, 30)
    move(a, b, 20)
    print(a.total)
    print(b.total)
    var xs = [1]
    var ys = [2]
    drain(xs, ys)
    print(xs.count)
    print(ys.count)
    print(ys[2])
    return
}
"#,
    );
    assert_eq!(output, "50\n55\n2\n3\n9\n");
}

/// A `borrow mut` argument may be a field or an element, not just a bare name,
/// and the write lands exactly there.
#[test]
fn a_mutable_borrow_of_a_nested_place_agrees() {
    let output = assert_parity(
        r#"
struct Cell {
    var value: Int
}

struct Grid {
    var origin: Cell
    var cells: [Cell]
}

function bump(cell: borrow mut Cell) {
    cell.value = cell.value + 1
    return
}

@Main
function main() {
    var grid = Grid { origin: Cell { value: 10 }, cells: [Cell { value: 20 }, Cell { value: 30 }] }
    bump(grid.origin)
    bump(grid.cells[1])
    bump(grid.cells[1])
    print(grid.origin.value)
    print(grid.cells[0].value)
    print(grid.cells[1].value)
    return
}
"#,
    );
    assert_eq!(output, "11\n20\n32\n");
}

/// A `borrow mut` parameter passed onward as a `borrow mut` argument keeps
/// writing through to the original binding.
#[test]
fn a_mutable_borrow_passed_onward_still_reaches_the_caller() {
    let output = assert_parity(
        r#"
struct Counter {
    var n: Int
}

function inner(c: borrow mut Counter) {
    c.n = c.n * 2
    return
}

function outer(c: borrow mut Counter) {
    inner(c)
    inner(c)
    return
}

@Main
function main() {
    var counter = Counter { n: 3 }
    outer(counter)
    print(counter.n)
    return
}
"#,
    );
    assert_eq!(output, "12\n");
}

/// A value moved in each arm of a branch runs the same on every backend.
///
/// Newly accepted by the move checker's branch merge; the arms are
/// alternatives, so exactly one move happens per call and the value reaches
/// exactly one callee.
#[test]
fn moving_the_same_value_in_sibling_branches_agrees() {
    let output = assert_parity(
        r#"
struct Mesh {
    var id: Int
    var label: String
}

function asMetal(mesh: Mesh) -> String {
    return mesh.label + "/metal"
}

function asVulkan(mesh: Mesh) -> String {
    return mesh.label + "/vulkan"
}

function describe(useMetal: Bool, mesh: Mesh) -> String {
    if useMetal {
        return asMetal(move mesh)
    } else {
        return asVulkan(move mesh)
    }
}

@Main
function main() {
    print(describe(true, Mesh { id: 1, label: "a" }))
    print(describe(false, Mesh { id: 2, label: "b" }))
    return
}
"#,
    );
    assert_eq!(output, "a/metal\nb/vulkan\n");
}
