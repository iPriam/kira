//! What a construct family obliges its declarations to provide: the Construct
//! 2.0 top-level requirement members `@Required let` and `@Required function`,
//! and every refusal that reports one left undischarged.

use crate::tests::{codes, library_codes};

/// A family requirement written as a bodyless `@Required function`, implemented
/// by the backed declaration and called through the concrete value.
#[test]
fn a_required_function_implemented_by_its_declaration_checks_clean() {
    assert!(
        codes(
            r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    function area() -> Int {
        return side * side
    }
}

@Main function main() {
    print(Square(side: 5).area())
    return
}
"#,
        )
        .is_empty()
    );
}

/// A requirement has no body, so there is nothing for a declaration to inherit:
/// leaving it unimplemented is reported rather than silently satisfied by the
/// family's empty block.
#[test]
fn a_required_function_left_unimplemented_is_refused() {
    assert_eq!(
        library_codes(
            r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    let width: Int = 0
}
"#,
        ),
        vec!["KSEM234"]
    );
}

/// A requirement that wrote a result type means it: an implementation returning
/// something else does not conform.
#[test]
fn a_required_function_result_type_is_enforced() {
    assert_eq!(
        library_codes(
            r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    function area() -> String {
        return "big"
    }
}
"#,
        ),
        vec!["KSEM235"]
    );
}

/// A requirement's parameters are enforced whether or not it wrote a result
/// type.
#[test]
fn a_required_function_parameter_list_is_enforced() {
    assert_eq!(
        library_codes(
            r#"
construct Shape {
    @Required function scaled(factor: Int)
}

construct Square(side: Int) extends Shape {
    function scaled() -> Int {
        return side
    }
}
"#,
        ),
        vec!["KSEM235"]
    );
}

/// A requirement written without `-> T` constrains the name and parameters only:
/// Kira has no top type to write there, and there is no body to make it `Void`,
/// so two declarations may answer with two different types.
#[test]
fn a_required_function_without_a_result_type_leaves_the_result_to_the_declaration() {
    assert!(
        library_codes(
            r#"
construct Case {
    @Required function outcome()
}

construct Counted(number: Int) extends Case {
    function outcome() -> Int {
        return number
    }
}

construct Named(text: String) extends Case {
    function outcome() -> String {
        return text
    }
}
"#,
        )
        .is_empty()
    );
}

/// A requirement that says nothing about its result cannot be dispatched through
/// the family value: there is no one type the call could have.
#[test]
fn calling_an_unconstrained_requirement_through_the_family_value_is_refused() {
    assert!(
        codes(
            r#"
construct Case {
    @Required function outcome()
}

construct Counted(number: Int) extends Case {
    function outcome() -> Int {
        return number
    }
}

@Main function main() {
    let any: some Case = Counted(number: 3)
    print(any.outcome())
    return
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM241")
    );
}

/// A requirement that wrote a result type dispatches through the family value
/// like any other family method.
#[test]
fn a_requirement_with_a_result_type_dispatches_through_the_family_value() {
    assert!(
        codes(
            r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    function area() -> Int {
        return side * side
    }
}

construct Strip(length: Int) extends Shape {
    function area() -> Int {
        return length
    }
}

@Main function main() {
    let shape: some Shape = Square(side: 4)
    print(shape.area())
    return
}
"#,
        )
        .is_empty()
    );
}

/// A backed declaration discharges obligations; it does not state them. A
/// bodyless member there would be an implementation that does nothing.
#[test]
fn a_required_function_on_a_backed_declaration_is_refused() {
    assert!(
        library_codes(
            r#"
construct Shape {
    @Required function area() -> Int
}

construct Square(side: Int) extends Shape {
    @Required function area() -> Int
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM249")
    );
}

/// `requires { … }` states the same obligation `@Required function` does, and a
/// declaration that discharges it checks clean.
#[test]
fn a_requires_section_states_the_same_obligation() {
    assert!(
        codes(
            r#"
construct Drawable {
    requires {
        function draw() -> Int
    }
}

construct Sprite() extends Drawable {
    let base: Int = 7
    function draw() -> Int { return base }
}

@Main
function main() {
    print(Sprite().draw())
    return
}
"#,
        )
        .is_empty()
    );
}

/// A declaration leaving a `requires` entry undischarged is reported by the
/// same code an undischarged `@Required function` is, because there is only one
/// requirement kind behind the two spellings.
#[test]
fn a_requires_entry_left_undischarged_is_reported() {
    assert!(
        codes(
            r#"
construct Drawable {
    requires {
        function draw() -> Int
    }
}

construct Sprite() extends Drawable {
    let base: Int = 7
}

@Main
function main() {
    print(Sprite().base)
    return
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM234")
    );
}

/// A `requires` section on a *backed* declaration is refused for the reason the
/// `@Required` annotation is there: a declaration implements requirements, it
/// does not state them.
#[test]
fn a_requires_section_on_a_backed_declaration_is_refused() {
    assert!(
        codes(
            r#"
construct Drawable {
    requires {
        function draw() -> Int
    }
}

construct Sprite() extends Drawable {
    requires {
        function extra() -> Int
    }
    function draw() -> Int { return 1 }
}

@Main
function main() {
    print(Sprite().draw())
    return
}
"#,
        )
        .iter()
        .any(|code| code == "KSEM249")
    );
}
