//! Semantic analysis of classes: flattening, override checking, ambiguity, and
//! the subtyping this port deliberately does not have.

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

/// A field typed by a name declared later gets the move-it-above fix, and the
/// message names each owner by its own keyword — a class owner called a
/// `struct` would send the reader looking for a declaration that is not there.
#[test]
fn a_forward_referenced_field_type_is_explained_for_a_class_too() {
    let reported = diagnostics(&format!(
        "class Wallet {{ var card: Card = Card() }}\nclass Card {{ var id: Int = 1 }}\n{MAIN}"
    ));
    let messages: Vec<&str> = reported
        .iter()
        .filter(|diagnostic| diagnostic.code == Some("KSEM051"))
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert_eq!(
        messages,
        vec![
            "class `Wallet` cannot hold a `Card` because `Card` is declared later in the file; \
             move `Card` above `Wallet`"
        ]
    );

    // The same shape with structs keeps saying `struct`.
    let reported = diagnostics(&format!(
        "struct Wallet {{ var card: Card }}\nstruct Card {{ var id: Int }}\n{MAIN}"
    ));
    assert!(
        reported
            .iter()
            .any(|diagnostic| diagnostic.code == Some("KSEM051")
                && diagnostic.message.starts_with("struct `Wallet`"))
    );
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
fn a_subclass_is_not_assignable_to_its_parents_type() {
    // No subtyping: a class instance's static type is always its dynamic type,
    // which is what makes the per-class method copy total. Admitting this would
    // reintroduce the dispatch question the whole design avoids.
    assert_eq!(
        codes(
            "class Base { let a: Int = 1 }\nclass Child extends Base {}\n\
             function take(b: Base) -> Int { return b.a }\n\
             @Main function main() { print(take(move Child())) return }"
        ),
        vec!["KSEM063"]
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
