//! The `@FFI.Extern` seam: what an accepted foreign declaration records, what a
//! call to one type-checks against, and every refusal the frontend carries.
//!
//! Seamless C-FFI is new Kira design — the oracle has no foreign-call concept —
//! so these tests are the specification of what the marker means. Every refusal
//! is checked by code and, where the program is otherwise clean, proved to be
//! the *only* diagnostic reported, so a rule is never mistaken for a cascade.

use super::*;
use kira_runtime_abi::{ForeignAbi, ForeignType};
use kira_semantics_model::HirProgram;
use kira_semantics_model::hir::{Callee, HirExpr};

/// The analyzed program of a single-file application.
fn program(text: &str) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), Vec::new());
    analyzed(&db, source)
}

/// Whether the program contains a call resolved to a foreign callable.
fn has_foreign_call(program: &HirProgram) -> bool {
    program.exprs.iter().any(|(_, expr)| {
        matches!(
            expr,
            HirExpr::Call {
                callee: Callee::Foreign(_),
                ..
            }
        )
    })
}

const ADD: &str = "@FFI.Extern { library: ffimath; symbol: kira_ffi_add; abi: c; }\n\
     function add(a: I32, b: I32) -> I32;\n\
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
    let text = "@FFI.Extern { library: l; symbol: greet; abi: c; }\n\
                function greet(name: CString) -> I32;\n\
                @Main function main() { let s = \"hi\" print(greet(s)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    // A string literal reaches it too.
    let literal = "@FFI.Extern { library: l; symbol: greet; abi: c; }\n\
                   function greet(name: CString) -> I32;\n\
                   @Main function main() { print(greet(\"hi\")) return }";
    assert!(
        diagnostics(literal).is_empty(),
        "{:?}",
        diagnostics(literal)
    );
}

#[test]
fn a_raw_ptr_round_trips_between_two_foreign_calls() {
    let text = "@FFI.Extern { library: l; symbol: make; abi: c; }\n\
                function makePtr() -> RawPtr;\n\
                @FFI.Extern { library: l; symbol: consume; abi: c; }\n\
                function usePtr(p: RawPtr);\n\
                @Main function main() { let p = makePtr() usePtr(p) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign.len(), 2);
}

// ----- structural / parser boundary ---------------------------------------

#[test]
fn a_bodyless_ordinary_function_is_a_parse_error() {
    let text = "@Main function main() { return }\nfunction f() -> I32;";
    assert!(codes(text).contains(&"KPAR055"), "{:?}", codes(text));
}

#[test]
fn an_extern_with_a_body_is_a_parse_error() {
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f() -> I32 { return 1 }";
    assert!(codes(text).contains(&"KPAR054"), "{:?}", codes(text));
}

// ----- annotation block meaning -------------------------------------------

fn extern_add(block: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ {block} }} function ffiAdd(a: I32, b: I32) -> I32;"
    )
}

#[test]
fn a_missing_required_field_is_refused() {
    assert_eq!(codes(&extern_add("library: l; abi: c;")), vec!["KSEM180"]);
}

#[test]
fn a_duplicate_field_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l; library: m; symbol: s; abi: c;")),
        vec!["KSEM179"]
    );
}

#[test]
fn an_unknown_field_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l; symbol: s; abi: c; bogus: x;")),
        vec!["KSEM178"]
    );
}

#[test]
fn a_non_c_abi_is_refused() {
    assert_eq!(
        codes(&extern_add("library: l; symbol: s; abi: rust;")),
        vec!["KSEM181"]
    );
}

// ----- annotation conflicts -----------------------------------------------

fn extern_with_marker(marker: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ library: l; symbol: s; abi: c; }} {marker} \
         function ffiAdd(a: I32) -> I32;"
    )
}

#[test]
fn ffi_extern_conflicts_with_each_execution_and_entry_annotation() {
    for marker in ["@Main", "@Runtime", "@Native", "@Export"] {
        assert_eq!(
            codes(&extern_with_marker(marker)),
            vec!["KSEM177"],
            "`@FFI.Extern` with {marker} must be one KSEM177 conflict"
        );
    }
}

// ----- signature type mapping ---------------------------------------------

fn extern_param(ty: &str) -> String {
    format!(
        "@Main function main() {{ return }}\n\
         @FFI.Extern {{ library: l; symbol: s; abi: c; }} function f(a: {ty}) -> I32;"
    )
}

#[test]
fn a_bare_int_parameter_is_refused() {
    assert_eq!(codes(&extern_param("Int")), vec!["KSEM182"]);
}

#[test]
fn a_bare_float_parameter_is_refused() {
    assert_eq!(codes(&extern_param("Float")), vec!["KSEM182"]);
}

#[test]
fn a_string_in_the_signature_is_refused() {
    assert_eq!(codes(&extern_param("String")), vec!["KSEM182"]);
}

#[test]
fn an_array_parameter_is_refused() {
    assert_eq!(codes(&extern_param("[I32]")), vec!["KSEM182"]);
}

#[test]
fn a_callback_parameter_is_refused() {
    assert_eq!(codes(&extern_param("(I32) -> I32")), vec!["KSEM182"]);
}

#[test]
fn a_generic_type_in_the_signature_is_refused() {
    // The written `Opt<I32>` is a generic *shape*, refused before it is even
    // resolved, so no undeclared-`Opt` diagnostic joins it.
    assert_eq!(codes(&extern_param("Opt<I32>")), vec!["KSEM182"]);
}

#[test]
fn a_multi_field_struct_parameter_is_refused() {
    let text = "struct Pt { let x: I32\nlet y: I32 }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(p: Pt) -> I32;";
    assert_eq!(codes(text), vec!["KSEM182"]);
}

#[test]
fn a_single_scalar_field_struct_crosses_as_its_field() {
    // A C handle struct — one scalar member — is passed in a register exactly
    // like that member, so it crosses the seam as the field's `U32` and carries
    // the struct to rebuild on both sides.
    let text = "struct Handle { var id: U32 }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(h: Handle) -> Handle;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    let row = &program.foreign[0];
    // The wire signature names the scalar, never the struct.
    assert_eq!(row.signature.parameters(), &[ForeignType::U32]);
    assert_eq!(row.signature.result(), ForeignType::U32);
    // The wrapper on each side records that the scalar is a rebuilt handle.
    assert_eq!(row.param_wrappers.len(), 1);
    assert!(row.param_wrappers[0].is_some());
    assert!(row.result_wrapper.is_some());
    assert_eq!(row.param_wrappers[0], row.result_wrapper);
}

#[test]
fn a_struct_whose_one_field_is_a_bare_int_is_refused() {
    // `Int` has no fixed C width, so a struct wrapping one is not a handle: it
    // falls to the ordinary aggregate refusal rather than crossing silently.
    let text = "struct Loose { var n: Int }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(h: Loose) -> I32;";
    assert_eq!(codes(text), vec!["KSEM182"]);
}

#[test]
fn a_cstring_result_is_refused() {
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f() -> CString;";
    assert_eq!(codes(text), vec!["KSEM182"]);
}

// ----- CString seam-only rule ---------------------------------------------

#[test]
fn a_cstring_local_is_refused() {
    let text = "@Main function main() { let x: CString = \"hi\" print(1) return }";
    assert_eq!(codes(text), vec!["KSEM176"]);
}

#[test]
fn a_cstring_struct_field_is_refused() {
    let text = "struct S { let p: CString }\n@Main function main() { return }";
    assert_eq!(codes(text), vec!["KSEM176"]);
}

#[test]
fn a_cstring_ordinary_parameter_is_refused() {
    let text = "function f(p: CString) { return }\n@Main function main() { return }";
    // Every code reported is the seam refusal and nothing else. An ordinary
    // parameter's type is resolved twice — once for the signature, once for the
    // body local — so the seam refusal, like any parameter-type error, is
    // reported at both, which is a property of parameter resolution rather than
    // of this rule.
    let codes = codes(text);
    assert!(!codes.is_empty());
    assert!(
        codes.iter().all(|code| *code == "KSEM176"),
        "only the seam refusal, got {codes:?}"
    );
}

#[test]
fn a_raw_ptr_is_allowed_as_an_ordinary_local() {
    // `RawPtr` is a normal scalar — no seam restriction — so a foreign result
    // bound to a local is clean.
    let text = "@FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function makePtr() -> RawPtr;\n\
                @Main function main() { let p = makePtr() print(1) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

// ----- call argument checking ---------------------------------------------

#[test]
fn a_string_passed_to_a_non_cstring_parameter_is_a_type_error() {
    let text = "@FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function takes(n: I32) -> I32;\n\
                @Main function main() { print(takes(\"hi\")) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

#[test]
fn a_non_string_passed_to_a_cstring_parameter_is_a_type_error() {
    let text = "@FFI.Extern { library: l; symbol: greet; abi: c; }\n\
                function greet(name: CString) -> I32;\n\
                @Main function main() { print(greet(42)) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

// ----- name collisions ----------------------------------------------------

#[test]
fn a_foreign_name_may_not_collide_with_a_user_function() {
    let text = "function dup() -> I32 { return 1 }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function dup() -> I32;\n\
                @Main function main() { print(dup()) return }";
    assert!(codes(text).contains(&"KSEM184"), "{:?}", codes(text));
}

#[test]
fn two_foreign_functions_may_not_share_a_name() {
    let text = "@FFI.Extern { library: l; symbol: s; abi: c; } function dup() -> I32;\n\
                @FFI.Extern { library: l; symbol: t; abi: c; } function dup() -> I32;\n\
                @Main function main() { return }";
    assert!(codes(text).contains(&"KSEM185"), "{:?}", codes(text));
}
