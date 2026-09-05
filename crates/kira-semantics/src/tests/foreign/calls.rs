//! Calls across the seam: what an argument may be, what the result is, and
//! the ownership a `retains:` parameter takes.

use super::*;

const ADD: &str = "@FFI.Extern { library: ffimath, symbol: kira_ffi_add, abi: c }\n\
     function add(a: I32, b: I32) -> I32\n\
     @Main function main() { print(add(20, 22)) return }";

#[test]
fn the_add_example_type_checks_and_records_one_foreign_row() {
    assert!(diagnostics(ADD).is_empty(), "{:?}", diagnostics(ADD));
    let program = program(ADD);
    assert_eq!(program.foreign.len(), 1);
    let row = &program.foreign[0];
    assert_eq!(row.kira_name, "add");
    assert_eq!(row.library, "ffimath");
    assert_eq!(row.symbol, "kira_ffi_add");
    assert_eq!(row.abi, ForeignAbi::C);
    assert_eq!(
        row.signature.parameters(),
        &[ForeignType::I32, ForeignType::I32]
    );
    assert_eq!(row.signature.result(), ForeignType::I32);
    // The call in `main` resolves to the foreign callable, not a user function.
    assert!(has_foreign_call(&program));
}

#[test]
fn a_string_argument_reaches_a_cstring_parameter() {
    // The one explicit coercion: a Kira `String` is accepted where a `CString`
    // parameter is expected, and the caller keeps its `String` (no `move`).
    let text = "@FFI.Extern { library: l, symbol: greet, abi: c }\n\
                function greet(name: CString) -> I32\n\
                @Main function main() { let s = \"hi\" print(greet(s)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    // A string literal reaches it too.
    let literal = "@FFI.Extern { library: l, symbol: greet, abi: c }\n\
                   function greet(name: CString) -> I32\n\
                   @Main function main() { print(greet(\"hi\")) return }";
    assert!(
        diagnostics(literal).is_empty(),
        "{:?}",
        diagnostics(literal)
    );
}

#[test]
fn a_raw_ptr_round_trips_between_two_foreign_calls() {
    let text = "@FFI.Extern { library: l, symbol: make, abi: c }\n\
                function makePtr() -> RawPtr\n\
                @FFI.Extern { library: l, symbol: consume, abi: c }\n\
                function usePtr(p: RawPtr)\n\
                @Main function main() { let p = makePtr() usePtr(p) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign.len(), 2);
}

// ----- call argument checking ---------------------------------------------

#[test]
fn a_string_passed_to_a_non_cstring_parameter_is_a_type_error() {
    let text = "@FFI.Extern { library: l, symbol: s, abi: c }\n\
                function takes(n: I32) -> I32\n\
                @Main function main() { print(takes(\"hi\")) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

#[test]
fn a_non_string_passed_to_a_cstring_parameter_is_a_type_error() {
    let text = "@FFI.Extern { library: l, symbol: greet, abi: c }\n\
                function greet(name: CString) -> I32\n\
                @Main function main() { print(greet(42)) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

#[test]
fn a_retained_cstring_consumes_an_owned_block() {
    let source = r#"
@FFI.Extern { library: fixture, symbol: keep, abi: c, retains: text }
function keep(text: CString): Void

@Main
function main() {
    let text = "kept"
    keep(move text)
    return
}
"#;
    assert!(diagnostics(source).is_empty(), "{:?}", diagnostics(source));
    let program = program(source);
    assert!(program.foreign[0].signature.is_retained(0));
    let argument = program
        .exprs
        .iter()
        .find_map(|(_, expr)| match expr {
            HirExpr::Call {
                callee: Callee::Foreign(_),
                args,
                ..
            } => args.first().copied(),
            _ => None,
        })
        .expect("the foreign call has one argument");
    assert!(matches!(program.expr(argument), HirExpr::CStringNew { .. }));
    assert_eq!(program.expr(argument).type_of(), Type::CBlock);
}

#[test]
fn a_retained_named_argument_requires_move() {
    let source = r#"
@FFI.Extern { library: fixture, symbol: keep, abi: c, retains: text }
function keep(text: CString): Void

@Main
function main() {
    let text = "kept"
    keep(text)
    return
}
"#;
    assert_eq!(codes(source), vec!["KSEM287"]);
}

#[test]
fn a_c_layout_value_moves_when_bound() {
    let source = r#"
@FFI.Struct { layout: c }
struct Desc { var label: CString }

@Main
function main() {
    let first = Desc { label = "owned" }
    let second = first
    let third = first
    return
}
"#;
    let found = codes(source);
    assert!(found.iter().any(|code| code == "KSEM107"), "{found:?}");
}
