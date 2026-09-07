//! One generic specialization never becomes another.
//!
//! `Result<Int, AppError>` and `Result<Any, AppError>` are two types, and no
//! position admits one where the other is written — not a `let`, an
//! assignment, a `return`, an argument, a field, a payload, or an element. A
//! program that wants the other specialization rebuilds it, which is ordinary
//! code and needs no rule here.

use super::{analyze_text, codes};
use kira_semantics_model::hir::{HirExpr, HirStmt};

/// The oracle's `Result` verbatim, plus the failure enum a use site needs.
const RESULT: &str = "enum AppError { NotFound Denied }\n\
                      enum Result<Value, Failure> {\n\
                          Ok(Value)\n\
                          Error(Failure)\n\
                      }\n\
                      function narrow(n: Int) -> Result<Int, AppError> {\n\
                          if n < 0 { return .Error(.NotFound) }\n\
                          return .Ok(n)\n\
                      }\n";

#[test]
fn a_narrow_instantiation_does_not_reach_a_wide_result() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             function wide(n: Int) -> Result<Any, AppError> {{\n\
                 return narrow(n)\n\
             }}\n\
             @Main function main() {{ return }}"
        )),
        vec!["KSEM032"]
    );
}

#[test]
fn no_position_that_admits_a_declared_type_admits_a_specialization_change() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             struct Wrapper {{ let inner: Result<Any, AppError> }}\n\
             enum Crate<Held> {{ Full(Held) Empty }}\n\
             function takes(outcome: Result<Any, AppError>) -> Int {{ return 1 }}\n\
             @Main function main() {{\n\
                 let annotated: Result<Any, AppError> = narrow(1)\n\
                 var slot: Result<Any, AppError> = narrow(2)\n\
                 slot = narrow(3)\n\
                 let argument = takes(narrow(4))\n\
                 let elements: [Result<Any, AppError>] = [narrow(5)]\n\
                 let field = Wrapper(inner: narrow(6))\n\
                 let payload: Crate<Result<Any, AppError>> = .Full(narrow(7))\n\
                 return\n\
             }}"
        )),
        vec![
            "KSEM020", "KSEM020", "KSEM022", "KSEM063", "KSEM105", "KSEM224", "KSEM123"
        ]
    );
}

#[test]
fn nothing_special_cases_the_name_result() {
    // A user's own template is refused by the identical path, and it does not
    // have to be shaped like `Result` to be.
    assert_eq!(
        codes(
            "enum Crate<Held> { Full(Held) Empty }\n\
             function crated(n: Int) -> Crate<String> {\n\
                 if n < 0 { return .Empty }\n\
                 return .Full(\"held\")\n\
             }\n\
             function wide(n: Int) -> Crate<Any> { return crated(n) }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM032"]
    );
}

#[test]
fn a_nested_specialization_is_refused_like_a_flat_one() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             function outer(n: Int) -> Result<Result<Int, AppError>, AppError> {{\n\
                 return .Ok(narrow(n))\n\
             }}\n\
             function wide(n: Int) -> Result<Result<Any, AppError>, AppError> {{\n\
                 return outer(n)\n\
             }}\n\
             @Main function main() {{ return }}"
        )),
        vec!["KSEM032"]
    );
}

#[test]
fn an_array_of_instantiations_stays_invariant() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             function wide() -> [Result<Any, AppError>] {{\n\
                 let xs: [Result<Int, AppError>] = [narrow(1)]\n\
                 return xs\n\
             }}\n\
             @Main function main() {{ return }}"
        )),
        vec!["KSEM032"]
    );
}

#[test]
fn two_templates_of_the_same_shape_stay_unrelated() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             enum Outcome<Value, Failure> {{ Ok(Value) Error(Failure) }}\n\
             function wide(n: Int) -> Outcome<Any, AppError> {{ return narrow(n) }}\n\
             @Main function main() {{ return }}"
        )),
        vec!["KSEM032"]
    );
}

#[test]
fn a_type_argument_never_changes_width() {
    // `U8` reaches an `Int` *position* because an integer literal has to. A
    // type argument is not that position.
    assert_eq!(
        codes(
            "enum Crate<Held> { Full(Held) Empty }\n\
             function bytes() -> Crate<U8> { return .Full(7) }\n\
             function wide() -> Crate<Int> { return bytes() }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM032"]
    );
}

#[test]
fn a_hand_written_enum_is_not_an_instantiation() {
    assert_eq!(
        codes(
            "enum Crate<Held> { Full(Held) Empty }\n\
             enum Held { Full(Int) Empty }\n\
             function plain() -> Held { return .Full(1) }\n\
             function wide() -> Crate<Any> { return plain() }\n\
             @Main function main() { return }"
        ),
        vec!["KSEM032"]
    );
}

/// The rebuild a program writes instead: unpack one specialization, build the
/// other. Every payload crosses into `Any` at its own erasure site.
#[test]
fn a_rebuild_carries_a_value_between_specializations() {
    assert!(
        codes(&format!(
            "{RESULT}\
             function widen(outcome: Result<Int, AppError>) -> Result<Any, AppError> {{\n\
                 match outcome {{\n\
                     Ok(value) -> {{ return .Ok(value) }}\n\
                     Error(failure) -> {{ return .Error(failure) }}\n\
                 }}\n\
             }}\n\
             @Main function main() {{ return }}"
        ))
        .is_empty()
    );
}

/// A value already of the declared instantiation crosses nothing: no rewrite
/// stands between it and the position that declared it.
#[test]
fn a_value_already_that_wide_crosses_nothing() {
    let program = analyze_text(&format!(
        "{RESULT}\
         function keep(value: Result<Any, AppError>) -> Result<Any, AppError> {{\n\
             return value\n\
         }}\n\
         @Main function main() {{ return }}"
    ));
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
        matches!(program.expr(*value), HirExpr::Local { .. }),
        "the parameter is returned as it stands, found {:?}",
        program.expr(*value)
    );
}
