//! VM == LLVM == hybrid for array literals, indexing, `.count`, `.append`,
//! index writes, nested paths, arrays of structs, `for`-in, and the two index
//! traps.
//!
//! These are the shapes the wasm suite already proves across the VM and both
//! address widths; running the same source through the real `kirac` on all
//! three of its backends is the other half of the parity claim — the native
//! backend frees an array through the runtime where wasm's bump allocator never
//! does, so a leak or a double free would show up as a divergence here.

use crate::{assert_parity, assert_trap_parity};

#[test]
fn a_literal_and_its_elements_agree() {
    assert_parity(
        r#"
@Main
function main() {
    let xs = [1, 2, 3]
    print(xs[0])
    print(xs[1])
    print(xs[2])
    print(xs.count)
    return
}
"#,
    );
}

/// The universal idiom: an empty literal grown by `append`. On the native
/// backend this reallocates the runtime's item block, so it is also the growth
/// test.
#[test]
fn appending_to_an_empty_array_agrees() {
    assert_parity(
        r#"
@Main
function main() {
    var xs: [Int] = []
    print(xs.count)
    xs.append(10)
    xs.append(20)
    xs.append(30)
    print(xs.count)
    print(xs[0] + xs[1] + xs[2])
    return
}
"#,
    );
}

/// Growth reallocates and copies, so a long run is where an off-by-one in the
/// capacity doubling would show up.
#[test]
fn many_appends_agree_across_several_growths() {
    assert_parity(
        r#"
@Main
function main() {
    var xs: [Int] = []
    for i in 0..50 {
        xs.append(i * i)
    }
    print(xs.count)
    var total = 0
    for x in xs {
        total = total + x
    }
    print(total)
    print(xs[0])
    print(xs[49])
    return
}
"#,
    );
}

#[test]
fn an_index_write_agrees() {
    assert_parity(
        r#"
@Main
function main() {
    var xs = [1, 2, 3]
    xs[1] = 99
    print(xs[0])
    print(xs[1])
    print(xs[2])
    return
}
"#,
    );
}

#[test]
fn string_elements_agree() {
    assert_parity(
        r#"
@Main
function main() {
    var names: [String] = []
    names.append("a")
    names.append("bb")
    for n in names {
        print(n)
    }
    print(names.count)
    return
}
"#,
    );
}

#[test]
fn float_and_bool_elements_agree() {
    assert_parity(
        r#"
@Main
function main() {
    let fs = [1.5, 2.5]
    print(fs[0] + fs[1])
    let bs = [true, false]
    print(bs[0])
    print(bs[1])
    return
}
"#,
    );
}

#[test]
fn nested_arrays_index_twice() {
    assert_parity(
        r#"
@Main
function main() {
    var grid: [[Int]] = [[1, 2], [3, 4]]
    print(grid[0][1])
    print(grid[1][0])
    grid[1][1] = 77
    print(grid[1][1])
    print(grid.count)
    print(grid[0].count)
    return
}
"#,
    );
}

/// A write through a path that mixes an index and two fields. The native walk
/// is a GEP chain with a runtime bounds check in the middle; if it rebuilt
/// anything instead of writing through, the read back would not see it.
#[test]
fn a_write_through_a_nested_path_agrees() {
    assert_parity(
        r#"
struct Inner { var x: Int }
struct Outer { var inner: Inner }

@Main
function main() {
    var rows: [Outer] = []
    rows.append(Outer { inner = Inner { x = 1 } })
    rows[0].inner.x = 77
    print(rows[0].inner.x)
    return
}
"#,
    );
}

/// `append` resolves a place, so a write through a field of an element has to
/// land in the array rather than in a copy that is discarded.
#[test]
fn appending_through_a_struct_field_agrees() {
    assert_parity(
        r#"
struct Bag { var xs: [Int] }

@Main
function main() {
    var b = Bag { xs = [] }
    b.xs.append(1)
    b.xs.append(2)
    print(b.xs.count)
    print(b.xs[0] + b.xs[1])
    return
}
"#,
    );
}

/// The question the design turned on: copying a struct must copy its array
/// field, not share the handle. The native backend clones through
/// `kira_rt_array_clone`, so both engines have to agree the original is
/// untouched — and neither may double-free the shared-looking block.
#[test]
fn copying_a_struct_does_not_alias_its_array_field() {
    assert_parity(
        r#"
struct Bag { var xs: [Int] }

function grow(b: Bag) -> Int {
    var local = b
    local.xs.append(99)
    return local.xs.count
}

@Main
function main() {
    var original = Bag { xs = [1, 2] }
    print(grow(move original))
    return
}
"#,
    );
}

#[test]
fn an_array_of_structs_agrees() {
    assert_parity(
        r#"
struct Point { var x: Int; var y: Int }

@Main
function main() {
    var ps: [Point] = []
    ps.append(Point { x = 1, y = 2 })
    ps.append(Point { x = 3, y = 4 })
    var total = 0
    for p in ps {
        total = total + p.x + p.y
    }
    print(total)
    print(ps[1].x)
    return
}
"#,
    );
}

/// `for x in xs` reads and does not consume, so the array is still there
/// afterwards — and the loop variable is a copy, so writing to what it names
/// cannot perturb the iteration.
#[test]
fn for_in_reads_its_array_and_leaves_it() {
    assert_parity(
        r#"
@Main
function main() {
    let xs = [1, 2, 3]
    var total = 0
    for x in xs {
        total = total + x
    }
    print(total)
    print(xs.count)
    return
}
"#,
    );
}

#[test]
fn for_in_over_an_empty_array_runs_zero_times() {
    assert_parity(
        r#"
@Main
function main() {
    var xs: [Int] = []
    for x in xs {
        print(x)
    }
    print(xs.count)
    return
}
"#,
    );
}

/// `continue` must not skip the cursor step, or the loop spins. Same argument
/// the range form's desugar makes; this is the array form's proof.
#[test]
fn break_and_continue_inside_for_in_agree() {
    assert_parity(
        r#"
@Main
function main() {
    let xs = [1, 2, 3, 4, 5, 6]
    var total = 0
    for x in xs {
        if x == 2 {
            continue
        }
        if x == 5 {
            break
        }
        total = total + x
    }
    print(total)
    return
}
"#,
    );
}

// ----- traps ---------------------------------------------------------

/// Out of range is a runtime trap on every backend: no output, non-zero exit.
#[test]
fn an_index_past_the_end_traps_the_same_way() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let xs = [1, 2, 3]
    print(xs[3])
    return
}
"#,
        "",
    );
}

#[test]
fn an_index_past_the_end_of_an_empty_array_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    var xs: [Int] = []
    print(xs[0])
    return
}
"#,
        "",
    );
}

/// A negative index is a *different* trap from one past the end, but every
/// backend still refuses it the same way a caller sees: nothing printed,
/// non-zero exit.
#[test]
fn a_negative_index_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    let xs = [1, 2, 3]
    var at = 0
    at = at - 1
    print(xs[at])
    return
}
"#,
        "",
    );
}

#[test]
fn an_out_of_range_index_write_traps() {
    assert_trap_parity(
        r#"
@Main
function main() {
    var xs = [1]
    xs[5] = 2
    print(xs[0])
    return
}
"#,
        "",
    );
}

/// Reading an element must not copy the array — on either engine.
///
/// Both used to: the VM's `LoadLocal` copied the whole array before
/// `ArrayGet` read one element, and the native backend cloned the base for the
/// same reason. That made a loop over `n` elements cost `O(n²)`, so 200,000
/// reads took seven seconds native and three minutes on the VM, and loading an
/// 18 MB mesh never finished at all.
///
/// The size is what makes this a regression test rather than a unit test: a
/// quadratic read is *correct*, just unusable, so only a program big enough to
/// notice can tell the two apart.
#[test]
fn reading_elements_does_not_copy_the_array() {
    assert_parity(
        r"
@Main function main() {
    var xs: [Int] = []
    var i = 0
    while i < 20000 {
        xs.append(i)
        i = i + 1
    }
    var sum = 0
    var j = 0
    while j < 20000 {
        sum = sum + xs[j]
        j = j + 1
    }
    // The sum pins that borrowing the base still reads every element
    // correctly, rather than merely quickly.
    print(String(sum))
    return
}
",
    );
}

/// Writing through an element still leaves other copies alone.
///
/// The borrow above hands out a *copy* of the element, so a value read out of
/// an array can never alias the array's own storage. Nothing about reading
/// faster is allowed to change that.
#[test]
fn an_element_read_out_is_independent_of_the_array() {
    assert_parity(
        r"
@Main function main() {
    var xs: [[Int]] = [[1, 2], [3, 4]]
    var first = xs[0]
    first.append(99)
    print(String(first.count))
    print(String(xs[0].count))
    return
}
",
    );
}

// ----- borrows -------------------------------------------------------

/// A `borrow mut` array given a second name is still the caller's array.
///
/// `var out = nodes` binds no value when `nodes` is a borrow — there is none to
/// bind, only the caller's storage — so an append through the second name has
/// to reach the first. Every engine used to copy here, agreeing with each other
/// and with nobody else, which is why this asserts the values rather than
/// leaving the three backends to confirm one answer among themselves.
#[test]
fn appending_through_a_rebound_borrow_reaches_the_caller() {
    let output = assert_parity(
        r#"
function appendOne(nodes: borrow mut [Int], value: Int) {
    var out = nodes
    out.append(value)
    return
}

function appendTwo(nodes: borrow mut [Int]) {
    var out = nodes
    appendOne(out, 10)
    appendOne(out, 20)
    print(out.count)
    return
}

@Main
function main() {
    var xs: [Int] = []
    appendTwo(xs)
    print(xs.count)
    print(xs[0])
    print(xs[1])
    return
}
"#,
    );
    assert_eq!(output, "2\n2\n10\n20\n");
}

/// A rebound borrow that is written *through* aliases; one that is rebound to
/// something else does not.
///
/// The second name is only the first when it stands for nothing else. Assigning
/// a whole new array to it makes it an ordinary local again, and the borrow it
/// started as must not see that value.
#[test]
fn a_rebound_borrow_that_is_reassigned_stops_aliasing() {
    let output = assert_parity(
        r#"
function replace(nodes: borrow mut [Int]) {
    var out = nodes
    out = [7, 8, 9]
    out.append(10)
    print(out.count)
    return
}

@Main
function main() {
    var xs: [Int] = [1]
    replace(xs)
    print(xs.count)
    print(xs[0])
    return
}
"#,
    );
    assert_eq!(output, "4\n1\n1\n");
}

/// Reading one field of an array element reads that field, not the element.
///
/// The element is left alone: its own handles are still its own, and writing
/// through the array afterwards is still visible. Lowered as written this
/// copies the whole element out to read one word of it, which is what a layout
/// pass doing it thousands of times a frame cannot afford — so this pins the
/// behaviour the fast path has to preserve.
#[test]
fn reading_one_field_of_an_element_leaves_the_element_alone() {
    let output = assert_parity(
        r#"
struct Node {
    var name: String
    var count: Int
}

@Main
function main() {
    var nodes: [Node] = [
        Node { name: "first" count: 1 },
        Node { name: "second" count: 2 }
    ]
    print(nodes[0].name)
    print(nodes[1].count)
    nodes[0].count = 10
    print(nodes[0].count)
    print(nodes[0].name)
    print(nodes.count)
    return
}
"#,
    );
    assert_eq!(output, "first\n2\n10\nfirst\n2\n");
}
