//! What one Kira type crosses the seam as, and what has no crossing.

use super::*;

#[test]
fn the_bare_scalars_cross_as_the_sixty_four_bit_c_types() {
    // `Int` and `Float` name the native 64-bit C scalar types directly.
    assert_eq!(codes(&extern_param("Int")), Vec::<String>::new());
    assert_eq!(codes(&extern_param("Float")), Vec::<String>::new());
}

#[test]
fn a_string_in_the_signature_is_refused() {
    assert_eq!(codes(&extern_param("String")), vec!["KSEM182"]);
}

#[test]
fn an_array_parameter_names_itself() {
    assert!(codes(&extern_param("[I32]")).is_empty());
}

#[test]
fn a_callback_parameter_is_refused() {
    assert_eq!(codes(&extern_param("(I32) -> I32")), vec!["KSEM182"]);
}

#[test]
fn a_generic_type_in_the_signature_is_judged_by_what_it_resolves_to() {
    // `Opt` is undeclared here, so the diagnostic names that unresolved type
    // rather than treating generic syntax as a foreign-boundary error.
    let codes = codes(&extern_param("Opt<I32>"));
    assert!(codes.iter().any(|code| code == "KSEM050"), "{codes:?}");
}

#[test]
fn a_multi_field_struct_parameter_is_refused() {
    let text = "struct Pt { let x: I32\nlet y: I32 }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(p: Pt) -> I32";
    assert_eq!(codes(text), vec!["KSEM182"]);
}

#[test]
fn a_single_scalar_field_struct_crosses_as_its_field() {
    // A C handle struct — one scalar member — is passed in a register exactly
    // like that member, so it crosses the seam as the field's `U32` and carries
    // the struct to rebuild on both sides.
    let text = "struct Handle { var id: U32 }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(h: Handle) -> Handle";
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
fn a_c_layout_struct_may_hold_the_bare_scalars() {
    // `Int` maps to int64_t and `Float` maps to double, so both have defined
    // C widths for layout.
    let text = "@FFI.Struct { layout: c }\n\
                struct Fine { var a: Float\n var n: Int }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(b: Fine) -> I32";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_multi_field_struct_without_the_annotation_is_still_refused() {
    // The annotation is the author's statement that this type mirrors a C
    // declaration. Without it, adding a Kira field would silently change what
    // the C function receives, so the plain struct keeps its refusal.
    let text = "struct Loose { var x: Float\n var y: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(p: Loose) -> I32";
    assert_eq!(codes(text), vec!["KSEM182"]);
    assert!(program(text).foreign_aggregates.is_empty());
}

#[test]
fn a_struct_whose_one_field_is_a_bare_int_is_a_handle() {
    // One 64-bit member, passed in a register exactly like the member — the C
    // single-member-struct handle. `Int` names a width now, so this crosses
    // rather than falling to the aggregate refusal.
    let text = "struct Handle { var n: Int }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(h: Handle) -> I32";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_cstring_result_is_accepted_and_is_a_string_in_kira() {
    // The callee returns a `const char*` it keeps and the seam copies the bytes,
    // so the Kira side of the call is an ordinary owned `String` — which is what
    // makes the result assignable to one and printable.
    let text = "@Main function main() { let s: String = f() print(s) return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f() -> CString";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn a_string_result_is_still_refused_at_the_seam() {
    // `String` is Kira's spelling, not C's: naming it at the seam says nothing
    // about the C type, which is why `CString` is the one that crosses.
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f() -> String";
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
    let text = "@FFI.Extern { library: l, symbol: s, abi: c }\n\
                function makePtr() -> RawPtr\n\
                @Main function main() { let p = makePtr() print(1) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// A payload-less enum crosses as its case's number, which is what a C enum is.
#[test]
fn a_payload_less_enum_is_a_foreign_parameter() {
    let source = r#"
enum Usage { Vertex Index Uniform }

@FFI.Extern { library: fixture, symbol: stride, abi: c }
function stride(usage: Usage): I32

function ask() -> Int {
    return stride(Usage.Index)
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// An enum that carries a payload is a tagged union, and C's own is a different
/// shape with a different layout — so it has no crossing rather than a lossy one.
#[test]
fn an_enum_with_a_payload_is_refused() {
    let source = r#"
enum Reading { None Value(Int) }

@FFI.Extern { library: fixture, symbol: take, abi: c }
function take(reading: Reading): I32
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM182"),
        "{:?}",
        library_codes(source)
    );
}

/// An array names itself in a signature rather than being spelled `RawPtr`.
#[test]
fn an_array_is_a_foreign_parameter() {
    let source = r#"
@FFI.Extern { library: fixture, symbol: sum, abi: c }
function sum(values: [F32], count: I32): F32

function ask(values: borrow [F32]) -> Float {
    return sum(values, values.count)
}
"#;
    assert!(
        library_codes(source).is_empty(),
        "{:?}",
        library_codes(source)
    );
}

/// An array *result* has no reading: a C function answers a pointer, and
/// nothing in that answer says how many elements are behind it.
#[test]
fn an_array_result_is_refused_for_having_no_length() {
    let source = r#"
@FFI.Extern { library: fixture, symbol: give, abi: c }
function give(): [F32]
"#;
    let codes = library_codes(source);
    assert!(codes.iter().any(|code| code == "KSEM182"), "{codes:?}");
}

/// An array of something C has no width for is refused by its element.
#[test]
fn an_array_of_a_non_seam_element_is_refused() {
    let source = r#"
@FFI.Extern { library: fixture, symbol: take, abi: c }
function take(values: [String]): I32
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM182"),
        "{:?}",
        library_codes(source)
    );
}

/// A generic instantiation is judged by what it instantiated to.
///
/// This instantiation resolves to a tagged union, so the diagnostic names the
/// resolved representation rather than generic syntax.
#[test]
fn a_generic_instantiation_is_refused_by_what_it_resolves_to() {
    let source = r#"
enum Wrapped<T> { Ok(T) Bad }

@FFI.Extern { library: fixture, symbol: take, abi: c }
function take(w: Wrapped<I32>): I32
"#;
    let diagnostics = library_diagnostics(source);
    let said = diagnostics
        .iter()
        .map(|diagnostic| diagnostic.message.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(said.contains("Wrapped<I32>"), "{said}");
    assert!(!said.contains("a generic type cannot"), "{said}");
}
