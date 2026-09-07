//! Analysis of `Any`, Kira's top type: what widens into it, what does not, and
//! where the erasure lands in the tree.

use super::{analyze_text, codes, diagnostics};
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
@FFI.Extern { library: l, symbol: host_take, abi: c }
function hostTake(value: Any) -> I32

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

/// `is` and `as` read an `Any`: a value whose type is already known has
/// nothing to ask, and a type no `Any` can hold can never answer.
#[test]
fn is_and_as_need_an_any_and_an_erasable_target() {
    let fine = "struct P { let x: Int }\n\
                @Main function main() {\n    let a: Any = P(x: 1)\n\
                if a is P { print((a as P).x) }\n    return\n}";
    assert!(diagnostics(fine).is_empty(), "{:?}", codes(fine));
    assert_eq!(
        codes("@Main function main() { let n = 1 print(n is Int) return }"),
        vec!["KSEM358"]
    );
    assert_eq!(
        codes("@Main function main() { let a: Any = 1 print(a is Any) return }"),
        vec!["KSEM359"]
    );
    assert_eq!(
        codes("@Main function main() { let a: Any = 1 let v: Void = a as Void return }"),
        vec!["KSEM359"]
    );
}

/// `value.type` answers for every inhabited value, and its descriptor has a
/// closed set of members.
#[test]
fn a_value_answers_with_its_runtime_type() {
    let ok = "struct Point { let x: Int = 1 }\n\
              @Main function main() {\n\
                  let p = Point()\n\
                  print(p.type.name)\n\
                  print(p.type.kind)\n\
                  print(p.type.package)\n\
                  print(p.type.arguments.count)\n\
                  print(p.type.conformances.count)\n\
                  let erased: Any = Point()\n\
                  print(erased.type == p.type)\n\
                  return\n\
              }";
    assert!(diagnostics(ok).is_empty(), "{:?}", codes(ok));
}

/// A `Void` call names no value, so it has no type to describe.
#[test]
fn a_void_expression_has_no_runtime_type() {
    let text = "function nothing() { return }\n\
                @Main function main() { let t = nothing().type return }";
    assert_eq!(codes(text), vec!["KSEM362"]);
}

/// The descriptor's members are the whole surface: fields, methods, and layout
/// stay compile-time facts.
#[test]
fn a_descriptor_has_no_member_beyond_the_documented_five() {
    let text = "struct Point { let x: Int = 1 }\n\
                @Main function main() { print(Point().type.fields) return }";
    assert_eq!(codes(text), vec!["KSEM363"]);
}

/// Two descriptors compare, and a descriptor does not compare with anything
/// else.
#[test]
fn descriptors_compare_only_with_descriptors() {
    let text = "struct Point { let x: Int = 1 }\n\
                @Main function main() { print(Point().type == 1) return }";
    assert!(!codes(text).is_empty());
}

/// A cast under `try` is a fallible step the enclosing `attempt` handles; the
/// same cast without one traps and needs no handler.
#[test]
fn a_cast_under_try_is_handled_like_any_other_failure() {
    let handled = "struct Point { let x: Int = 1 }\n\
                   @Main function main() {\n\
                       let boxed: Any = Point()\n\
                       attempt {\n\
                           let p = try boxed as Point\n\
                           print(p.x)\n\
                       } handle {\n\
                           Mismatch(actual) { print(actual.name) }\n\
                       }\n\
                       return\n\
                   }";
    assert!(diagnostics(handled).is_empty(), "{:?}", codes(handled));

    // The handler must cover the failure, exactly as it must for a call.
    let unhandled = "struct Point { let x: Int = 1 }\n\
                     @Main function main() {\n\
                         let boxed: Any = Point()\n\
                         attempt {\n\
                             let p = try boxed as Point\n\
                             print(p.x)\n\
                         } handle {\n\
                         }\n\
                         return\n\
                     }";
    assert!(
        codes(unhandled).contains(&"KSEM139".to_owned()),
        "{:?}",
        codes(unhandled)
    );

    // Outside an `attempt` a cast is not fallible, so `try` is refused there
    // for the reason it is always refused.
    let loose = "struct Point { let x: Int = 1 }\n\
                 @Main function main() {\n\
                     let boxed: Any = Point()\n\
                     let p = try boxed as Point\n\
                     print(p.x)\n\
                     return\n\
                 }";
    assert!(
        codes(loose).contains(&"KSEM137".to_owned()),
        "{:?}",
        codes(loose)
    );
}

/// A program may declare a `TypeCastError` of its own; the compiler's is a
/// different type and a tried cast still reports through its own.
#[test]
fn a_programs_own_cast_error_name_does_not_capture_the_compilers() {
    let text = "enum TypeCastError { Other }\n\
                struct Point { let x: Int = 1 }\n\
                @Main function main() {\n\
                    let boxed: Any = Point()\n\
                    attempt {\n\
                        let p = try boxed as Point\n\
                        print(p.x)\n\
                    } handle {\n\
                        Mismatch(actual) { print(actual.name) }\n\
                    }\n\
                    return\n\
                }";
    assert!(diagnostics(text).is_empty(), "{:?}", codes(text));
}
