//! Parity for the two ways a construct family states an obligation, and for
//! calling a member on a declaration's *name*.
//!
//! `requires { function … }` and `@Required function` produce the same member,
//! so what is proven here is that they also produce the same *execution*: the
//! declaration's own body, reached through the family's tag dispatcher, on vm,
//! llvm, and hybrid alike.
//!
//! `Sprite.draw()` is the second half. A construct-backed declaration names a
//! thing rather than a type, so the name alone is enough to call a member —
//! against a default construction from outside the declaration, and against
//! `self` from inside it. Those are two different receivers reached through one
//! spelling, which is exactly the kind of difference a backend can get wrong
//! silently: a backend that always defaulted would print the declared default
//! where the constructed value should show, and still run.
//!
//! Split from [`super::constructs`] rather than appended to it: that file is
//! already at the file-size ladder, and these cases are about the requirement
//! surface rather than about construction and content.

use crate::assert_parity;

/// A `requires` section, two declarations satisfying it, a sibling call through
/// the declaration name, and dispatch through the family value.
///
/// Differentially checked against the oracle's installed 1.7.3 `kira`, which
/// prints the same six lines.
#[test]
fn a_requires_section_dispatches_on_every_backend() {
    let output = assert_parity(
        r#"
construct Drawable {
    requires {
        function draw() -> Int
        function name() -> String
    }
}

construct Sprite() extends Drawable {
    let base: Int = 7

    function draw() -> Int {
        // A sibling reached through the declaration name runs against `self`,
        // so this reads the `base` the receiver was built with.
        return base + Sprite.offset()
    }

    function offset() -> Int {
        return 3
    }

    function name() -> String {
        return "sprite"
    }
}

construct Tile() extends Drawable {
    let side: Int = 4

    function draw() -> Int {
        return side * side
    }

    function name() -> String {
        return "tile"
    }
}

function describe(d: borrow Any Drawable) -> String {
    return d.name()
}

@Main
function main() {
    // From outside the declaration: a default construction.
    print(Sprite.draw())
    print(Tile.draw())
    // From a constructed value: the same body, the constructed `base`.
    let s = Sprite(base: 3)
    print(s.draw())
    print(describe(s))
    let items: [some Drawable] = [Sprite(base: 1), Tile(side: 2)]
    var total = 0
    for item in items {
        total = total + item.draw()
    }
    print(total)
    print(Sprite.name())
    return
}
"#,
    );
    assert_eq!(output, "10\n16\n6\nsprite\n8\nsprite\n");
}

/// A family mixing the section spelling with the annotation spelling, including
/// a `@Required let` discharged by a stored field on one declaration and a
/// computed member on the other.
///
/// The point is that nothing downstream can tell the two spellings apart: one
/// family states three obligations three ways, and every declaration satisfies
/// all of them through the same dispatchers.
///
/// The `requires`-plus-`@Required function` half was differentially checked
/// against the oracle's installed 1.7.3 `kira`, which prints the same
/// `[stored]`/`<computed>`/`11`. The `@Required let` half is deliberately
/// **wider** here than the oracle, which admits only a computed member for a
/// value requirement (`KSEM066`) and cannot read a computed member by bare name
/// from a sibling member (`KSEM012`). Discharging a value requirement with
/// either shape is this implementation's rule — the consistent one — and
/// `existentials::a_required_value_member_dispatches_across_field_and_computed_shapes`
/// is where it is pinned on its own.
#[test]
fn the_two_requirement_spellings_mix_in_one_family() {
    let output = assert_parity(
        r#"
construct Cell {
    requires {
        function render() -> String
    }
    @Required function weight() -> Int
    @Required let tag: String
}

construct Stored() extends Cell {
    let tag: String = "stored"
    function render() -> String { return "[" + tag + "]" }
    function weight() -> Int { return 2 }
}

construct Computed() extends Cell {
    let side: Int = 3
    let tag: String { "computed" }
    function render() -> String { return "<" + tag + ">" }
    function weight() -> Int { return side * side }
}

@Main
function main() {
    let cells: [some Cell] = [Stored(), Computed()]
    var total = 0
    for cell in cells {
        print(cell.render())
        print(cell.tag)
        total = total + cell.weight()
    }
    print(total)
    print(Stored.render())
    return
}
"#,
    );
    assert_eq!(
        output,
        "[stored]\nstored\n<computed>\ncomputed\n11\n[stored]\n"
    );
}

/// A family that extends another, driven entirely through the parent's type.
///
/// Nothing in `drive` names `Task` or `Job`. It holds `[Any Runnable]` and calls
/// a member the parent declared, so what is proven is that a declaration backed
/// by a child family really is a variant of the parent's enum and reaches its
/// own body through the parent's dispatcher.
///
/// The narrowed `render` is the second half: `Task` promises a `String` where
/// `Runnable` promised an `Any`, so every backend has to carry the concrete
/// answer up to the erased result rather than hand back a bare string.
#[test]
fn a_child_family_dispatches_through_its_parent_on_every_backend() {
    let output = assert_parity(
        r#"
construct Runnable {
    @Required function label() -> String
    @Required function render() -> Any
    function announce() -> String { return "run " + label() }
}

construct Task extends Runnable {
    @Required function render() -> String
}

construct Job extends Runnable {}

construct Fetch() extends Task {
    label { return "fetch" }
    render { return "[fetch]" }
}

construct Render() extends Job {
    label { return "render" }
    function render() -> Any { return 7 }
}

function drive(items: borrow [Any Runnable]) -> Int {
    var count = 0
    for item in items {
        print(item.announce())
        count = count + 1
    }
    return count
}

@Main
function main() {
    let all: [Any Runnable] = [Fetch(), Render()]
    print(drive(all))
    let narrowed: Any Task = Fetch()
    print(narrowed.render())
    return
}
"#,
    );
    assert_eq!(output, "run fetch\nrun render\n2\n[fetch]\n");
}
