//! Parity for labeled (named) call arguments.
//!
//! A label binds an argument to the parameter it names, so every backend must
//! resolve the same permutation to the same positional call — the VM, the
//! native build, and the hybrid bundle cannot disagree on which value reached
//! which parameter.

use crate::assert_parity;

#[test]
fn labeled_arguments_bind_by_name_on_every_backend() {
    let output = assert_parity(
        r#"
function measure(tree: Int, index: Int, available: Int) -> Int {
    return tree * 100 + index * 10 + available
}

@Main
function main() {
    // Declaration order, `:` binder.
    print(measure(tree: 1, index: 2, available: 3))
    // Reordered: the label, not the position, decides the binding.
    print(measure(available: 3, tree: 1, index: 2))
    // `=` is the canonical binder and produces the same call.
    print(measure(tree = 4, index = 5, available = 6))
    // A positional call still means the same thing.
    print(measure(7, 8, 9))
    return
}
"#,
    );
    assert_eq!(output, "123\n123\n456\n789\n");
}

#[test]
fn labeled_method_arguments_bind_by_name_on_every_backend() {
    let output = assert_parity(
        r#"
struct Grid {
    var w: Int
    function at(row: Int, col: Int) -> Int {
        return row * self.w + col
    }
    function first() -> Int {
        // An implicit self-method call, labeled and reordered.
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
    assert_eq!(output, "34\n34\n12\n");
}

#[test]
fn a_reordered_labeled_call_evaluates_in_resolved_parameter_order() {
    // A labeled call lowers to the same positional call an unlabeled one does,
    // so its arguments evaluate in the order the parameters were declared, not
    // the order they were written. The value that matters is that every backend
    // resolves that one order identically: `first` (note(2)) runs before
    // `second` (note(1)), so `2` prints before `1`, and the result is the same
    // `21` on the VM, the native build, and the hybrid bundle.
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
    assert_eq!(output, "2\n1\n21\n");
}
