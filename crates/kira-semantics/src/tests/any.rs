//! Analysis of `Any`, Kira's top type: what widens into it, what does not, and
//! where the erasure lands in the tree.

use super::{analyze_text, codes};
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirStmt};

/// The top type is spellable in every position a type is written in.
#[test]
fn any_is_spellable_wherever_a_type_is() {
    assert!(
        codes(
            r#"
struct Slot {
    let held: Any
}

enum Held {
    Nothing
    Something(Any)
}

function keep(value: Any) -> Any {
    return value
}

@Main
function main() {
    let annotated: Any = 1
    let inArray: [Any] = [2, "three"]
    let inStruct = Slot(held: 4.5)
    let inEnum: Held = .Something(true)
    let returned: Any = keep(move annotated)
    return
}
"#,
        )
        .is_empty()
    );
}

/// Every type widens into `Any`, including the ones that own heap storage.
#[test]
fn every_type_widens_into_any() {
    assert!(
        codes(
            r#"
struct Point { let x: Int }

enum Shade { Dim }

@Main
function main() {
    let fromInt: Any = 1
    let fromWidth: Any = 3
    let fromFloat: Any = 2.5
    let fromBool: Any = true
    let fromString: Any = "text"
    let fromStruct: Any = Point(x: 1)
    let ints: [Int] = [1]
    let fromArray: Any = move ints
    let shade: Shade = .Dim
    let fromEnum: Any = move shade
    let alreadyErased: Any = 9
    let fromAny: Any = move alreadyErased
    return
}
"#,
        )
        .is_empty()
    );
}

/// `Any` does not narrow. Without a recovery form, letting it would be a
/// reinterpretation of a boxed value rather than a conversion.
#[test]
fn any_does_not_narrow_to_a_concrete_type() {
    assert_eq!(
        codes(
            r#"
@Main
function main() {
    let erased: Any = 1
    let back: Int = move erased
    return
}
"#,
        ),
        vec!["KSEM020"]
    );
}

/// `Void` is the one type that does not widen: it names no value, so there is
/// nothing to erase.
#[test]
fn void_does_not_widen_into_any() {
    assert_eq!(
        codes(
            r#"
function nothing() {
    return
}

function erase() -> Any {
    return nothing()
}

@Main
function main() {
    return
}
"#,
        ),
        vec!["KSEM032"]
    );
}

/// An erased value has no rendering, so `print` refuses it — the same answer a
/// struct and an array get, and for a stronger reason: there is no type here to
/// pin a format for.
#[test]
fn print_refuses_an_erased_value() {
    assert_eq!(
        codes(
            r#"
@Main
function main() {
    let erased: Any = 1
    print(erased)
    return
}
"#,
        ),
        vec!["KSEM081"]
    );
}

/// The crossing is a node in the tree, not something a backend re-derives.
#[test]
fn the_erasure_is_recorded_where_the_value_crosses() {
    let program = analyze_text(
        r#"
@Main
function main() {
    let erased: Any = 7
    return
}
"#,
    );
    let main = program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("the entrypoint analyzed");
    let &first = main.body.first().expect("the `let` is the first statement");
    let HirStmt::Let { init, .. } = program.stmt(first) else {
        panic!("expected a `let`, found {:?}", program.stmt(first))
    };
    let HirExpr::IntoAny { from, .. } = program.expr(*init) else {
        panic!(
            "a value crossing into `Any` is wrapped, found {:?}",
            program.expr(*init)
        )
    };
    // The type it had is carried, because it is the only thing that says what
    // the erased value owns.
    assert_eq!(*from, Type::INT);
}

/// A value that is *already* erased is not wrapped twice.
#[test]
fn an_already_erased_value_crosses_nothing() {
    let program = analyze_text(
        r#"
function keep(value: Any) -> Any {
    return value
}

@Main
function main() {
    return
}
"#,
    );
    let keep = program
        .functions
        .iter()
        .find(|function| function.name == "keep")
        .expect("it analyzed");
    let &first = keep
        .body
        .first()
        .expect("the `return` is the only statement");
    let HirStmt::Return { value: Some(value) } = program.stmt(first) else {
        panic!("expected a `return` with a value")
    };
    assert!(
        !matches!(program.expr(*value), HirExpr::IntoAny { .. }),
        "an `Any` returned from an `Any` result needs no crossing"
    );
}

/// `Any` cannot cross the C seam: an erased value has no type for C to read it
/// back as.
#[test]
fn any_cannot_cross_the_c_seam() {
    assert_eq!(
        codes(
            r#"
@FFI.Extern { library: l; symbol: host_take; abi: c; }
function hostTake(value: Any) -> I32;

@Main
function main() {
    return
}
"#,
        ),
        vec!["KSEM182"]
    );
}

/// A construct family may name `Any` as a requirement's result, and a member
/// answering with its own concrete type satisfies it.
///
/// The family here is a user's own, with names this compiler has never heard of
/// — which is the point: nothing about `Any` in a requirement is special to
/// Foundation's `Test`.
#[test]
fn a_family_requirement_may_return_the_top_type() {
    assert!(
        codes(
            r#"
construct Measure {
    @Required function reading() -> Any
}

construct Depth() extends Measure {
    reading { 12 }
}

construct Label() extends Measure {
    reading { "deep" }
}

@Main
function main() {
    let measured: Any = Depth().reading
    let labelled: Any = Label().reading
    print("measured")
    return
}
"#,
        )
        .is_empty()
    );
}

/// A family that declares no such member leaves the shorthand meaning what it
/// always did: the family type.
///
/// This is the `body { … }` case, and it is the fallback rather than a rule of
/// its own — which is why the member here is not called `body`.
#[test]
fn a_shorthand_the_family_never_declared_still_yields_the_family_type() {
    assert!(
        codes(
            r#"
construct Node {
    @Required let tree: Any Node
    function depth() -> Int {
        return tree.depth() + 1
    }
}

construct Leaf() extends Node {
    function depth() -> Int {
        return 1
    }
}

construct Branch() extends Node {
    tree {
        Leaf()
    }
}

@Main
function main() {
    print(Branch().depth())
    return
}
"#,
        )
        .is_empty()
    );
}
