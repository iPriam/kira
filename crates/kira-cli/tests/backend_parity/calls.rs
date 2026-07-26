//! Parity for labeled (named) call arguments.
//!
//! A label binds an argument to the parameter it names, so every backend must
//! resolve the same permutation to the same positional call — the VM, the
//! native build, and the hybrid bundle cannot disagree on which value reached
//! which parameter.

use crate::{assert_module_parity, assert_parity};

#[test]
fn parameter_defaults_fill_omitted_arguments_on_every_backend() {
    // A default is resolved once into HIR and reused as an ordinary argument,
    // so every backend runs the same fully-applied call — whether the argument
    // was omitted positionally, omitted by label, or passed outright.
    let output = assert_parity(
        r#"
function step(base: Int, by: Int = 10, tag: Int = 100) -> Int {
    return base + by + tag
}

struct Counter {
    var at: Int
}

function bump(counter: borrow Counter, by: Int = 3) -> Int {
    return counter.at + by
}

@Main
function main() {
    // Every default taken.
    print(step(1))
    // One default taken.
    print(step(1, 20))
    // No default taken.
    print(step(1, 20, 300))
    // Labels bind nothing, so this fills `by` and leaves `tag` to its default —
    // it does not skip `by`, whatever the label says.
    print(step(base: 1, tag: 300))
    // A method call taking its default, then passing it.
    let c = Counter { at = 5 }
    print(bump(c))
    print(bump(c, 40))
    return
}
"#,
    );
    assert_eq!(output, "111\n121\n321\n401\n8\n45\n");
}

/// An omitted argument is the value the callee's module resolved, even when
/// the call site shares none of that module's imports.
#[test]
fn a_cross_module_parameter_default_agrees() {
    let output = assert_module_parity(
        "import definitions\n\
         @Main function main() {\n\
             print(seeded(1))\n\
             return\n\
         }",
        &[
            ("helper", "function helperValue() -> Int { return 41 }"),
            (
                "definitions",
                "import helper as H\n\
                 function seeded(base: Int, extra: Int = H.helperValue()) -> Int {\n\
                     return base + extra\n\
                 }",
            ),
        ],
    );
    assert_eq!(output.as_bytes(), b"42\n");
}

#[test]
fn argument_labels_are_decorative_on_every_backend() {
    let output = assert_parity(
        r#"
function measure(tree: Int, index: Int, available: Int) -> Int {
    return tree * 100 + index * 10 + available
}

@Main
function main() {
    // Declaration order, `:` binder: the label agrees with the position.
    print(measure(tree: 1, index: 2, available: 3))
    // Reordered. The *position* decides, so this is `measure(3, 1, 2)` and the
    // labels are three pieces of wrong documentation.
    print(measure(available: 3, tree: 1, index: 2))
    // `=` is the canonical binder and binds the same way.
    print(measure(tree = 4, index = 5, available = 6))
    // A bare positional call.
    print(measure(7, 8, 9))
    // Labeling only the middle argument is allowed and changes nothing.
    print(measure(1, index = 2, 3))
    return
}
"#,
    );
    assert_eq!(output, "123\n312\n456\n789\n123\n");
}

#[test]
fn labels_on_a_method_call_are_decorative_on_every_backend() {
    let output = assert_parity(
        r#"
struct Grid {
    var w: Int
    function at(row: Int, col: Int) -> Int {
        return row * self.w + col
    }
    function first() -> Int {
        // An implicit self-method call, labeled and reordered: `at(2, 1)`.
        return self.at(col: 2, row: 1)
    }
}

@Main
function main() {
    let g = Grid { w = 10 }
    print(g.at(row: 3, col: 4))
    print(g.at(col: 4, row: 3))
    print(g.first())
    return
}
"#,
    );
    assert_eq!(output, "34\n43\n21\n");
}

#[test]
fn a_labeled_call_evaluates_in_written_order() {
    // Labels bind nothing, so this is `record(note(1), note(2))`: the arguments
    // evaluate left to right as written — `1` before `2` — and bind in that same
    // order, giving `12`. Every backend agrees on both the order and the result.
    let output = assert_parity(
        r#"
function record(first: Int, second: Int) -> Int {
    return first * 10 + second
}

function note(n: Int) -> Int {
    print(n)
    return n
}

@Main
function main() {
    print(record(second: note(1), first: note(2)))
    return
}
"#,
    );
    assert_eq!(output, "1\n2\n12\n");
}
