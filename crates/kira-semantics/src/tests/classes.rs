//! Semantic analysis of classes: flattening, override checking, ambiguity, and
//! how far subtyping reaches.
//!
//! A subclass is admitted at a call argument, where the callee is specialized
//! for its concrete class and an override still wins. It is refused everywhere
//! else — a parent-typed binding, an array of the parent — because there is
//! nothing yet to carry the concrete class across the widening, and running the
//! parent's method on a child is a wrong answer rather than a limitation.

use super::{codes, diagnostics};

/// A `@Main` that does nothing, appended so a case is a whole program.
const MAIN: &str = "@Main function main() { return }";

#[test]
fn a_class_flattens_its_parents_fields_and_methods() {
    assert!(
        codes(
            "class Account { var balance: Int = 100\n let rate: Int = 2\n \
               function gross() -> Int { return self.balance * self.rate } }\n\
             class Savings extends Account { override let rate = 5 }\n\
             @Main function main() { print(Savings().gross()) return }"
        )
        .is_empty()
    );
}

/// A class field may name a class declared later, exactly as a struct field may
/// name a later struct: class *names* join the table before any field resolves,
/// and only the flattening that `extends` needs waits for inheritance order.
#[test]
fn a_class_field_may_name_a_class_declared_later() {
    let reported = diagnostics(&format!(
        "class Wallet {{ var card: Card = Card() }}\nclass Card {{ var id: Int = 1 }}\n{MAIN}"
    ));
    assert!(reported.is_empty(), "{reported:?}");
}

/// And in the other direction: a struct field may name a class.
///
/// This is what an application's configuration struct looks like — a record of
/// settings holding the class the host hands it — and it resolves whichever file
/// each was written in.
#[test]
fn a_struct_field_may_name_a_class() {
    let reported = diagnostics(&format!(
        "struct Config {{ var wallet: Wallet }}\nclass Wallet {{ var id: Int = 1 }}\n{MAIN}"
    ));
    assert!(reported.is_empty(), "{reported:?}");
}

/// Lifting the ordering means a value cycle can now be *spelled* through a
/// class, so it is caught outright rather than left to recurse forever.
#[test]
fn a_value_cycle_through_a_class_is_refused() {
    let reported = diagnostics(&format!(
        "struct Holder {{ var card: Card }}\nclass Card {{ var holder: Holder }}\n{MAIN}"
    ));
    let codes: Vec<&str> = reported
        .iter()
        .filter_map(kira_diagnostics::Diagnostic::code_text)
        .collect();
    assert_eq!(codes, vec!["KSEM052"], "{reported:?}");
}

/// A *struct* field may name a struct declared later in the same file: struct
/// collection is two-phase, so every name is registered before any field is
/// resolved. This is what a flat package scope means for a single file — the
/// same rule that lets a field name a sibling in another file.
#[test]
fn a_struct_field_may_name_a_struct_declared_later_in_the_file() {
    assert!(
        diagnostics(&format!(
            "struct Wallet {{ var card: Card }}\nstruct Card {{ var id: Int }}\n{MAIN}"
        ))
        .is_empty()
    );
}

/// A struct that reaches itself through by-value fields has no finite size, so
/// it is broken and reported (`KSEM052`) rather than left to recurse a backend
/// to death. Only the field closing the cycle is blamed; an array or enum field
/// is indirection and breaks the cycle instead.
#[test]
fn a_struct_value_cycle_is_rejected() {
    // Direct self-reference by value.
    assert_eq!(
        codes(&format!("struct Node {{ var next: Node }}\n{MAIN}")),
        vec!["KSEM052"]
    );
    // A mutual by-value cycle is caught at its closing edge, reported once.
    assert_eq!(
        codes(&format!(
            "struct A {{ var b: B }}\nstruct B {{ var a: A }}\n{MAIN}"
        )),
        vec!["KSEM052"]
    );
    // The same shape through an array breaks the cycle: a `[Node]` is a heap
    // handle, so a finite `Node` can hold one.
    assert!(diagnostics(&format!("struct Node {{ var next: [Node] }}\n{MAIN}")).is_empty());
}

#[test]
fn an_inheritance_cycle_is_reported_once() {
    assert_eq!(
        codes(&format!(
            "class Left extends Right {{}}\nclass Right extends Left {{}}\n{MAIN}"
        )),
        vec!["KSEM064"]
    );
}

#[test]
fn a_duplicated_parent_is_reported() {
    assert_eq!(
        codes(&format!(
            "struct Base {{ let a: Int = 0 }}\nclass Child extends Base, Base {{}}\n{MAIN}"
        )),
        vec!["KSEM065"]
    );
}

#[test]
fn an_override_must_match_the_signature_it_overrides() {
    assert_eq!(
        codes(&format!(
            "class Base {{ function ping(value: Int) -> Int {{ return value }} }}\n\
             class Child extends Base {{ override function ping() -> Int {{ return 1 }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM066"]
    );
}

#[test]
fn an_override_that_overrides_nothing_is_reported() {
    assert_eq!(
        codes(&format!(
            "class Base {{ let a: Int = 1 }}\n\
             class Child extends Base {{ override function ping() -> Int {{ return 1 }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM073"]
    );
    assert_eq!(
        codes(&format!(
            "class Base {{ let a: Int = 1 }}\nclass Child extends Base {{ override let b = 2 }}\n\
             {MAIN}"
        )),
        vec!["KSEM072"]
    );
}

/// An override may restate the inherited type, and doing so changes nothing.
#[test]
fn an_override_may_restate_the_inherited_type() {
    assert!(
        codes(
            "class Account { var balance: Int = 100\n let rate: Int = 2\n \
               function gross() -> Int { return self.balance * self.rate } }\n\
             class Savings extends Account { override let rate: Int = 5 }\n\
             @Main function main() { print(Savings().gross()) return }"
        )
        .is_empty()
    );
}

/// A restatement that disagrees is refused: the override is chosen by name, so
/// a wrong type there says the author meant a different field.
#[test]
fn an_override_that_restates_a_different_type_is_reported() {
    assert_eq!(
        codes(&format!(
            "class Base {{ let rate: Int = 1 }}\n\
             class Child extends Base {{ override let rate: String = \"5\" }}\n\
             {MAIN}"
        )),
        vec!["KSEM059"]
    );
}

#[test]
fn a_bare_name_two_parents_declare_is_ambiguous() {
    assert_eq!(
        codes(&format!(
            "struct Left {{ let value: Int = 1 }}\nstruct Right {{ let value: Int = 2 }}\n\
             class Child extends Left, Right {{ function read() -> Int {{ return value }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM068"]
    );
}

#[test]
fn a_bare_call_two_parents_declare_is_ambiguous() {
    assert_eq!(
        codes(&format!(
            "class Left {{ function ping() -> Int {{ return 1 }} }}\n\
             class Right {{ function ping() -> Int {{ return 2 }} }}\n\
             class Child extends Left, Right {{ function read() -> Int {{ return ping() }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM067"]
    );
}

#[test]
fn qualifying_an_ambiguous_member_resolves_it() {
    // The ambiguity is about the bare name only; naming the parent is the fix,
    // and it has to actually work or the diagnostic would be a dead end.
    assert!(
        codes(&format!(
            "class Left {{ let v: Int = 1\n function ping() -> Int {{ return 1 }} }}\n\
             class Right {{ let v: Int = 2\n function ping() -> Int {{ return 2 }} }}\n\
             class Child extends Left, Right {{\n\
               function read() -> Int {{ return Left.v + Right.v + Left.ping() + Right.ping() }} }}\n\
             {MAIN}"
        ))
        .is_empty()
    );
}

#[test]
fn a_qualifier_that_is_not_a_parent_is_refused() {
    assert_eq!(
        codes(&format!(
            "struct Left {{ let value: Int = 1\n function read() -> Int {{ return value }} }}\n\
             struct Right {{ let value: Int = 2\n function read() -> Int {{ return value }} }}\n\
             class Child extends Left {{ function callOther() -> Int {{ return Right.read() }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM069"]
    );
}

#[test]
fn a_parent_qualifier_outside_a_method_is_refused() {
    assert_eq!(
        codes(&format!(
            "class Base {{ function ping() -> Int {{ return 1 }} }}\n\
             function free() -> Int {{ return Base.ping() }}\n{MAIN}"
        )),
        vec!["KSEM069"]
    );
}

#[test]
fn a_subclass_is_refused_where_no_specialization_can_pick_the_override() {
    // Subtyping is admitted at a call argument because the callee is
    // specialized for the concrete class there. A binding annotated with the
    // parent has no such copy: `a.speak()` would resolve against `Base` and run
    // the parent's method on a `Child`. Refused until a value can carry its
    // class across the widening and dispatch on it — a wrong answer is worse
    // than the refusal it would replace.
    assert_eq!(
        codes(
            "class Base { function ping() -> Int { return 1 } }\n\
             class Child extends Base { override function ping() -> Int { return 2 } }\n\
             @Main function main() { let b: Base = Child {} print(b.ping()) return }"
        ),
        vec!["KSEM020"]
    );
}

#[test]
fn an_array_of_a_parent_is_refused_a_subclass_element() {
    assert_eq!(
        codes(
            "class Base { function ping() -> Int { return 1 } }\n\
             class Child extends Base { override function ping() -> Int { return 2 } }\n\
             @Main function main() { let all: [Base] = [Child {}] print(all.count) return }"
        ),
        vec!["KSEM105"]
    );
}

#[test]
fn a_subclass_is_assignable_to_its_parents_type() {
    // A class flattens its parent fields first, so `Child` has `Base`'s prefix.
    // The specialized call body keeps the receiver statically typed as `Child`,
    // so an override wins without runtime dispatch.
    assert_eq!(
        codes(
            "class Base { let a: Int = 1 }\nclass Child extends Base {}\n\
             function take(b: Base) -> Int { return b.a }\n\
             @Main function main() { print(take(move Child())) return }"
        ),
        Vec::<String>::new()
    );
}

#[test]
fn redeclaring_an_inherited_field_needs_override() {
    assert_eq!(
        codes(&format!(
            "class Base {{ let a: Int = 1 }}\nclass Child extends Base {{ let a: Int = 2 }}\n{MAIN}"
        )),
        vec!["KSEM074"]
    );
}

#[test]
fn a_constructor_checks_its_argument_count_and_types() {
    assert_eq!(
        codes(
            "class Holder { let n: Int }\n\
             @Main function main() { let h = Holder() print(h.n) return }"
        ),
        vec!["KSEM062"]
    );
    assert_eq!(
        codes(
            "class Holder { let n: Int }\n\
             @Main function main() { let h = Holder(\"x\") print(h.n) return }"
        ),
        vec!["KSEM063"]
    );
}

#[test]
fn an_unknown_parent_is_reported() {
    assert_eq!(
        codes(&format!("class Child extends Nope {{}}\n{MAIN}")),
        vec!["KSEM003"]
    );
}

#[test]
fn a_class_may_extend_a_struct_and_inherit_its_methods() {
    assert!(
        codes(
            "struct Base { let tag: Int = 11\n function label() -> Int { return self.tag * 2 } }\n\
             class Derived extends Base { override let tag = 21 }\n\
             @Main function main() { print(Derived().label()) return }"
        )
        .is_empty()
    );
}

#[test]
fn an_extend_block_may_add_methods_to_a_class() {
    // What lets a class outgrow one file: its body is minted once, but its
    // methods may be declared anywhere.
    assert_eq!(
        codes(&format!(
            "class Meter {{ var ticks: Int = 0 }}\n\
             extend Meter {{ function reading() -> Int {{ return self.ticks }} }}\n\
             {MAIN}"
        )),
        Vec::<String>::new()
    );
}

#[test]
fn an_extend_block_may_not_redeclare_a_method_the_class_body_has() {
    // Refused rather than resolved by file order. Two definitions of one
    // method, one of them in a file the reader may not have open, is a coin
    // toss dressed up as a rule.
    assert_eq!(
        codes(&format!(
            "class Meter {{\n\
             \x20   var ticks: Int = 0\n\
             \x20   function reading() -> Int {{ return self.ticks }}\n\
             }}\n\
             extend Meter {{ function reading() -> Int {{ return 0 }} }}\n\
             {MAIN}"
        )),
        vec!["KSEM257"]
    );
}

#[test]
fn an_extend_naming_neither_a_family_nor_a_class_is_reported() {
    assert_eq!(
        codes(&format!(
            "extend Nowhere {{ function reading() -> Int {{ return 0 }} }}\n{MAIN}"
        )),
        vec!["KSEM238"]
    );
}
