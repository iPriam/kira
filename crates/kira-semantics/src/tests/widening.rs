//! Widening one generic instantiation into another whose type arguments are
//! `Any`.
//!
//! Two things to pin. That the widening is admitted wherever a declared type is
//! — a `let`, an assignment, a `return`, an argument, a field, a payload, an
//! element — and that it is admitted *nowhere else*: an array of instantiations
//! stays invariant, nothing narrows back, and two templates that happen to have
//! the same shape stay unrelated.
//!
//! The other half — that the widened value behaves identically afterwards on
//! every engine — is not a diagnostic and cannot be checked here. It lives in
//! `kira-cli`'s `backend_parity/widening.rs`.

use super::{analyze_text, codes};
use kira_semantics_model::Type;
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
fn a_narrow_instantiation_reaches_a_wide_result() {
    assert!(
        codes(&format!(
            "{RESULT}\
             function wide(n: Int) -> Result<Any, AppError> {{\n\
                 return narrow(n)\n\
             }}\n\
             @Main function main() {{ return }}"
        ))
        .is_empty()
    );
}

#[test]
fn every_position_that_admits_a_declared_type_admits_the_widening() {
    assert!(
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
        ))
        .is_empty()
    );
}

#[test]
fn nothing_special_cases_the_name_result() {
    // A user's own template widens by the identical path, and it does not have
    // to be shaped like `Result` to.
    assert!(
        codes(
            "enum Crate<Held> { Full(Held) Empty }\n\
             function crated(n: Int) -> Crate<String> {\n\
                 if n < 0 { return .Empty }\n\
                 return .Full(\"held\")\n\
             }\n\
             function wide(n: Int) -> Crate<Any> { return crated(n) }\n\
             @Main function main() { return }"
        )
        .is_empty()
    );
}

#[test]
fn widening_composes_with_itself() {
    // The type argument being widened is itself an instantiation.
    assert!(
        codes(&format!(
            "{RESULT}\
             function outer(n: Int) -> Result<Result<Int, AppError>, AppError> {{\n\
                 return .Ok(narrow(n))\n\
             }}\n\
             function wide(n: Int) -> Result<Result<Any, AppError>, AppError> {{\n\
                 return outer(n)\n\
             }}\n\
             @Main function main() {{ return }}"
        ))
        .is_empty()
    );
}

#[test]
fn an_array_of_instantiations_stays_invariant() {
    // `[Int]` is not `[Any]`, and this adds no exception for an element that
    // happens to be a generic enum: the elements would each need rebuilding,
    // which is a different operation from widening the value in hand.
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
fn nothing_narrows_back() {
    assert_eq!(
        codes(&format!(
            "{RESULT}\
             function wide(n: Int) -> Result<Any, AppError> {{ return narrow(n) }}\n\
             function back(n: Int) -> Result<Int, AppError> {{ return wide(n) }}\n\
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
fn a_type_argument_never_widens_to_a_different_width() {
    // `U8` reaches an `Int` *position* because an integer literal has to. A
    // type argument is not that position: only `Any` widens one.
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
fn a_hand_written_enum_never_widens_into_an_instantiation() {
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

/// The crossing is a node in the tree, not something a backend re-derives.
#[test]
fn the_widening_is_recorded_where_the_value_crosses() {
    let program = analyze_text(&format!(
        "{RESULT}\
         @Main function main() {{\n\
             let wide: Result<Any, AppError> = narrow(1)\n\
             return\n\
         }}"
    ));
    let main = program
        .functions
        .iter()
        .find(|function| function.name == "main")
        .expect("the entrypoint analyzed");
    let &first = main.body.first().expect("the `let` is the first statement");
    let HirStmt::Let { init, .. } = program.stmt(first) else {
        panic!("expected a `let`, found {:?}", program.stmt(first))
    };
    let HirExpr::Widen { from, to, .. } = program.expr(*init) else {
        panic!(
            "a value crossing into a wider instantiation is wrapped, found {:?}",
            program.expr(*init)
        )
    };
    // Both rows are carried: the rebuild reads a variant list from each.
    assert!(matches!(from, Type::Enum(_)));
    assert!(matches!(to, Type::Enum(_)));
    assert_ne!(from, to);
}

/// A value already of the declared instantiation is not wrapped.
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
        !matches!(program.expr(*value), HirExpr::Widen { .. }),
        "a value already of the declared instantiation needs no crossing"
    );
}
