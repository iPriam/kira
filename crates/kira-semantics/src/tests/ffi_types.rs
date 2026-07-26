//! The struct-attached `@FFI.*` family: what each form becomes, the executable
//! zero-fill of a C-layout struct, and the typed refusals for the forms whose
//! runtime behavior is not yet executable.
//!
//! `@FFI.Alias`/`@FFI.Pointer` are aliases; `@FFI.Struct` is a real C-layout
//! struct with zero-filled construction; `@FFI.Array`/`@FFI.Callback` declare a
//! nominal type but refuse any *use* precisely. Every accepted case is proved
//! clean, and every refusal is checked by code, so a rule is never a cascade.

use super::*;
use kira_semantics_model::HirProgram;
use kira_semantics_model::hir::HirExpr;

/// The analyzed program of a single-file application.
fn program(text: &str) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), Vec::new());
    analyzed(&db, source)
}

/// The field expressions of the last `StructNew` the program built.
fn last_struct_new(program: &HirProgram) -> Vec<HirExpr> {
    let fields = program
        .exprs
        .iter()
        .rev()
        .find_map(|(_, expr)| match expr {
            HirExpr::StructNew { fields, .. } => Some(fields.clone()),
            _ => None,
        })
        .expect("a StructNew node");
    fields
        .into_iter()
        .map(|id| program.expr(id).clone())
        .collect()
}

// ----- aliases and pointers ------------------------------------------------

#[test]
fn an_ffi_alias_of_a_scalar_crosses_the_extern_seam() {
    // `Address` aliases `U64`, so an extern taking one is a `U64` at the seam.
    let text = "@FFI.Alias { target: U64; }\nstruct Address {}\n\
         @FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function use_addr(a: Address) -> U64;\n\
         @Main function main() { print(use_addr(0)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_pointer_crosses_the_extern_seam_as_a_raw_ptr() {
    // A native pointer is one machine word: `Handle_ptr` reaches the seam as
    // `RawPtr`, which is a legal foreign parameter.
    let text = "@FFI.Pointer { target: U8; ownership: borrowed; }\nstruct Handle_ptr {}\n\
         @FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function take(p: Handle_ptr) -> Handle_ptr;\n\
         @FFI.Extern { library: l; symbol: t; abi: c; }\n\
         function make() -> Handle_ptr;\n\
         @Main function main() { let p = take(make()) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_alias_may_chain_through_a_pointer() {
    // `Opaque` aliases `Handle_ptr`, itself an `@FFI.Pointer` → `RawPtr`.
    let text = "@FFI.Pointer { target: U8; ownership: borrowed; }\nstruct Handle_ptr {}\n\
         @FFI.Alias { target: Handle_ptr; }\nstruct Opaque {}\n\
         @FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function take(p: Opaque) -> Void;\n\
         @FFI.Extern { library: l; symbol: t; abi: c; }\n\
         function make() -> Opaque;\n\
         @Main function main() { take(make()) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_alias_colliding_with_a_builtin_is_rejected() {
    let text = "@FFI.Alias { target: U64; }\nstruct Int {}";
    assert!(codes(text).contains(&"KSEM130"), "{:?}", codes(text));
}

// ----- C-layout struct construction ---------------------------------------

#[test]
fn a_c_layout_struct_zero_fills_an_empty_literal() {
    let text = "@FFI.Struct { layout: c; }\n\
         struct V { var a: I32\n var b: Bool\n var c: F64 }\n\
         @Main function main() { let v = V {}\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let fields = last_struct_new(&program(text));
    assert!(
        matches!(
            fields.as_slice(),
            [HirExpr::Int(0), HirExpr::Bool(false), HirExpr::Float(f)] if *f == 0.0
        ),
        "{fields:?}"
    );
}

#[test]
fn a_c_layout_struct_zero_fills_omitted_fields_around_an_initializer() {
    let text = "@FFI.Struct { layout: c; }\n\
         struct V { var a: I32\n var b: I32\n var c: I32 }\n\
         @Main function main() { let v = V { b: 7 }\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let fields = last_struct_new(&program(text));
    assert!(
        matches!(
            fields.as_slice(),
            [HirExpr::Int(0), HirExpr::Int(7), HirExpr::Int(0)]
        ),
        "{fields:?}"
    );
}

#[test]
fn a_c_layout_struct_is_constructed_zeroed_by_paren_call() {
    let text = "@FFI.Struct { layout: c; }\n\
         struct V { var a: I32\n var b: Bool }\n\
         @Main function main() { let v = V()\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let fields = last_struct_new(&program(text));
    assert!(
        matches!(fields.as_slice(), [HirExpr::Int(0), HirExpr::Bool(false)]),
        "{fields:?}"
    );
}

#[test]
fn a_c_layout_paren_call_refuses_positional_arguments() {
    let text = "@FFI.Struct { layout: c; }\n\
         struct V { var a: I32 }\n\
         @Main function main() { let v = V(3)\n return }";
    assert!(codes(text).contains(&"KSEM189"), "{:?}", codes(text));
}

#[test]
fn a_nested_c_layout_field_zero_fills_recursively() {
    let text = "@FFI.Struct { layout: c; }\nstruct Inner { var x: I32 }\n\
         @FFI.Struct { layout: c; }\nstruct Outer { var inner: Inner\n var y: I32 }\n\
         @Main function main() { let o = Outer {}\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    // The outermost StructNew holds a nested StructNew and a zero.
    let outer = program(text)
        .exprs
        .iter()
        .rev()
        .find_map(|(_, expr)| match expr {
            HirExpr::StructNew { fields, .. } if fields.len() == 2 => Some(fields.clone()),
            _ => None,
        })
        .expect("the outer StructNew");
    assert!(matches!(
        program(text).expr(outer[0]),
        HirExpr::StructNew { .. }
    ));
}

#[test]
fn a_c_layout_field_with_no_zero_is_refused_when_omitted() {
    // A `String` is a Kira heap value with no C zero, so omitting it is refused
    // precisely rather than mis-initialized.
    let text = "@FFI.Struct { layout: c; }\nstruct V { var label: String }\n\
         @Main function main() { let v = V {}\n return }";
    assert!(codes(text).contains(&"KSEM186"), "{:?}", codes(text));
}

#[test]
fn a_pointer_field_zero_fills_to_null() {
    // `NULL` is what C zero-fills a pointer member to, so the omitted field has
    // a zero and the construction is clean.
    let text = "@FFI.Struct { layout: c; }\nstruct V { var p: RawPtr }\n\
         @Main function main() { let v = V {}\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn a_c_layout_initializer_still_type_checks_its_value() {
    let text = "@FFI.Struct { layout: c; }\nstruct V { var a: I32 }\n\
         @Main function main() { let v = V { a: true }\n return }";
    assert!(codes(text).contains(&"KSEM094"), "{:?}", codes(text));
}

// ----- deferred forms: array and callback ---------------------------------

#[test]
fn an_ffi_array_declaration_type_checks_as_a_field() {
    // Declaring the array and naming it as a field is fine; only a *use* is
    // refused.
    let text = "@FFI.Array { element: U8; count: 4; }\nstruct Bytes4 {}\n\
         @FFI.Struct { layout: c; }\nstruct Holder { var bytes: Bytes4 }\n\
         @Main function main() { let h = Holder {}\n return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn indexing_an_ffi_array_points_at_the_field_holding_its_elements() {
    let text = "@FFI.Array { element: U8; count: 4; }\nstruct Bytes4 {}\n\
         @FFI.Struct { layout: c; }\nstruct Holder { var bytes: Bytes4 }\n\
         @Main function main() { let h = Holder {}\n print(h.bytes[0])\n return }";
    assert!(codes(text).contains(&"KSEM244"), "{:?}", codes(text));
    // And the field it names does index.
    let through_field = "@FFI.Array { element: U8; count: 4; }\nstruct Bytes4 {}\n\
         @FFI.Struct { layout: c; }\nstruct Holder { var bytes: Bytes4 }\n\
         @Main function main() { let h = Holder {}\n print(h.bytes.elements.count)\n return }";
    assert!(
        diagnostics(through_field).is_empty(),
        "{:?}",
        diagnostics(through_field)
    );
}

#[test]
fn an_ffi_callback_as_an_extern_param_crosses_as_the_pointer_it_is() {
    let text = "@FFI.Callback { abi: c; params: [I32]; result: Void; }\nstruct Handler {}\n\
         @FFI.Extern { library: l; symbol: s; abi: c; }\n\
         function register(h: Handler) -> Void;\n\
         @Main function main() { return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_callback_declaration_alone_type_checks() {
    let text = "@FFI.Callback { abi: c; params: [I32, RawPtr]; result: I64; }\nstruct Cb {}\n\
         @Main function main() { return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}
