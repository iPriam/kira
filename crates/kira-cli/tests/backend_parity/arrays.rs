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
