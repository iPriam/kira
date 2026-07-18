//! Imports on the Web.
//!
//! A multi-file program reaches wasm as one `IrProgram` — the frontend resolves
//! every import before the IR exists — so there is nothing here for the wasm
//! lowering to have got wrong, no new node for a depth walker to miss, and no
//! new bump-allocated value. These cases are what turns that reasoning into a
//! result: the answer comes out of an imported module, and the VM, `wasm32`,
//! and `wasm64` all have to produce it.

use crate::assert_module_parity;

#[test]
fn a_module_function_runs_on_both_widths() {
    assert_module_parity(
        "import support\n@Main function main() { print(supportValue()) return }",
        &[("support", "function supportValue() -> Int { return 42 }")],
    );
}

#[test]
fn a_qualified_call_runs_on_both_widths() {
    assert_module_parity(
        "import support as Support\n@Main function main() { print(Support.twice(21)) return }",
        &[("support", "function twice(n: Int) -> Int { return n * 2 }")],
    );
}

/// A module's struct is heap-shaped work on this backend, where the allocator
/// is a bump pointer that never frees — so a module-declared struct is worth
/// running, not just type-checking.
#[test]
fn a_module_struct_runs_on_both_widths() {
    assert_module_parity(
        "import geometry as Geo\n\
         @Main function main() { let p: Geo.Point = Point { x: 3, y: 4 } print(p.area()) return }",
        &[(
            "geometry",
            "struct Point { let x: Int  let y: Int\n\
             function area() -> Int { return x * y } }",
        )],
    );
}

/// An array built in one module and consumed in another: the shared-handle
/// value crosses a file boundary that no longer exists by the time it is
/// lowered.
#[test]
fn a_module_array_runs_on_both_widths() {
    assert_module_parity(
        "import numbers\n\
         @Main function main() { let xs = threeNumbers() \
         for x in xs { print(x) } print(xs.count) return }",
        &[(
            "numbers",
            "function threeNumbers() -> [Int] { var xs: [Int] = [] \
             xs.append(1) xs.append(2) xs.append(3) return xs }",
        )],
    );
}

#[test]
fn a_transitive_module_runs_on_both_widths() {
    assert_module_parity(
        "import middle\n@Main function main() { print(middleValue()) return }",
        &[
            ("base", "function baseValue() -> Int { return 10 }"),
            (
                "middle",
                "import base\nfunction middleValue() -> Int { return baseValue() + 5 }",
            ),
        ],
    );
}

/// A trap raised inside an imported module is still the same trap on every
/// engine — the module boundary changes nothing about how a program stops.
#[test]
fn a_trap_inside_a_module_reports_the_same_on_both_widths() {
    assert_module_parity(
        "import numbers\n@Main function main() { print(pick(5)) return }",
        &[(
            "numbers",
            "function pick(i: Int) -> Int { var xs: [Int] = [] xs.append(1) return xs[i] }",
        )],
    );
}
