//! What a copy of an array shares, and what makes it stop sharing.
//!
//! Copying an array on the native backend copies no elements: the copy takes a
//! share of the item block, and a *write* through either array is what buys the
//! writer a block of its own. That is invisible if it is right and catastrophic
//! if it is wrong — a missed write path shows up as one array seeing another's
//! edit, or as two arrays freeing one set of strings.
//!
//! The language moves out of a place rather than copying it (`var b = a` on an
//! array is a move), so the copies that reach the runtime come from the shapes
//! here: a struct carrying an array field, a `borrow` read, a return value, and
//! an element read of an array of structs.
//!
//! Every expected value below was taken from the **oracle** toolchain, not from
//! the three backends agreeing with each other — sharing is exactly the kind of
//! change all three would get identically wrong.

use crate::assert_parity;

/// The base case: two copies of a struct sharing one array field, each written
/// a different way, and the original untouched by both.
///
/// A write is a store into an element and an append alike, and they take
/// different paths through the runtime — `slot_mut` and `push_slot` — so this
/// exercises one of each against a block held three times over.
#[test]
fn writing_one_copy_of_a_shared_array_leaves_the_others_alone() {
    let output = assert_parity(
        r#"
struct Bag { var xs: [String] }

function grow(b: Bag) -> Int {
    var local = b
    local.xs.append("new")
    local.xs[0] = "changed"
    return local.xs.count
}

@Main
function main() {
    var original = Bag { xs = ["keep"] }
    var one = original
    var two = original
    one.xs[0] = "one"
    two.xs.append("two")
    print(original.xs[0])
    print(original.xs.count)
    print(one.xs[0])
    print(one.xs.count)
    print(two.xs[0])
    print(two.xs.count)
    print(grow(move original))
    return
}
"#,
    );
    assert_eq!(output, "keep\n1\none\n1\nkeep\n2\n2\n");
}

/// A place walk that passes *through* two arrays has to unshare both.
///
/// `copy.cells[1].tags[0] = "z"` writes into an array reached through an
/// element of another array. Unsharing only the outer one would land the write
/// in the `tags` block `r` still reads; unsharing only the inner one would put
/// the fresh block in a `cells` element `r` also holds.
#[test]
fn a_write_through_two_arrays_unshares_both_of_them() {
    let output = assert_parity(
        r#"
struct Cell { var tags: [String] }
struct Row { var cells: [Cell] }

@Main
function main() {
    var r = Row { cells = [] }
    r.cells.append(Cell { tags = ["a"] })
    r.cells.append(Cell { tags = ["b", "c"] })
    var copy = r
    copy.cells[1].tags[0] = "z"
    copy.cells[0].tags.append("d")
    print(r.cells[1].tags[0])
    print(r.cells[0].tags.count)
    print(copy.cells[1].tags[0])
    print(copy.cells[0].tags.count)
    print(copy.cells.count)
    return
}
"#,
    );
    assert_eq!(output, "b\n1\nz\n2\n2\n");
}

/// Reading an element of an array of structs copies the element — its array
/// fields included — so a write through the copy is not a write into the array.
///
/// This is the shape a UI frame is made of: a layout pass pulls a node out of a
/// tree, edits it, and the tree is expected not to change underneath it.
#[test]
fn an_element_read_copies_the_arrays_inside_the_element() {
    let output = assert_parity(
        r#"
struct Node { var name: String var tags: [String] }

@Main
function main() {
    var ns: [Node] = []
    ns.append(Node { name = "a", tags = ["t1"] })
    var first = ns[0]
    first.tags[0] = "t2"
    first.name = "b"
    print(ns[0].name)
    print(ns[0].tags[0])
    print(first.name)
    print(first.tags[0])
    return
}
"#,
    );
    assert_eq!(output, "a\nt1\nb\nt2\n");
}

/// A borrowed read and a returned value share too, and a write after either
/// still reaches only the array it was made through.
///
/// The `for`-in reads the array through a lent borrow, which copies nothing at
/// all; the write between the two calls is what has to be visible to the
/// second, and the copy taken after it is what must not be.
#[test]
fn a_borrowed_read_sees_writes_and_a_copy_taken_after_does_not() {
    let output = assert_parity(
        r#"
struct Bag { var xs: [Int] }

function total(b: borrow Bag) -> Int {
    var sum = 0
    for x in b.xs {
        sum = sum + x
    }
    return sum
}

function make() -> Bag {
    return Bag { xs = [1, 2, 3] }
}

@Main
function main() {
    var b = make()
    print(total(b))
    b.xs[0] = 10
    print(total(b))
    print(b.xs.count)
    var c = b
    c.xs[1] = 20
    print(total(b))
    print(total(c))
    return
}
"#,
    );
    assert_eq!(output, "6\n15\n3\n15\n33\n");
}
