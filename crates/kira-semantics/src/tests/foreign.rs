//! The `@FFI.Extern` seam: what an accepted foreign declaration records, what a
//! call to one type-checks against, and every refusal the frontend carries.
//!
//! Seamless C-FFI is new Kira design — the oracle has no foreign-call concept —
//! so these tests are the specification of what the marker means. Every refusal
//! is checked by code and, where the program is otherwise clean, proved to be
//! the *only* diagnostic reported, so a rule is never mistaken for a cascade.

use super::*;
use kira_runtime_abi::{ForeignAbi, ForeignType, ForeignTypeSpec};
use kira_semantics_model::HirProgram;
use kira_semantics_model::hir::{Callee, HirExpr};

/// The analyzed program of a single-file application.
fn program(text: &str) -> HirProgram {
    let db = salsa::DatabaseImpl::new();
    let source =
        SourceProgram::application(&db, text.to_owned(), "test.kira".to_owned(), Vec::new());
    analyzed(&db, source).clone()
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
    assert!(
        codes(text).iter().any(|code| code == "KPAR055"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn an_extern_with_a_body_is_a_parse_error() {
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f() -> I32 { return 1 }";
    assert!(
        codes(text).iter().any(|code| code == "KPAR054"),
        "{:?}",
        codes(text)
    );
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
fn the_bare_scalars_cross_as_the_sixty_four_bit_c_types() {
    // `Int` *is* the 64-bit signed integer and `Float` the 64-bit float — there
    // is no second spelling for either — so both name a C type exactly and are
    // accepted. They were refused back when `I64`/`F64` existed to say the
    // width out loud; refusing them now would leave nothing to write.
    assert_eq!(codes(&extern_param("Int")), Vec::<String>::new());
    assert_eq!(codes(&extern_param("Float")), Vec::<String>::new());
}

#[test]
fn a_string_in_the_signature_is_refused() {
    assert_eq!(codes(&extern_param("String")), vec!["KSEM182"]);
}

#[test]
fn an_array_parameter_names_itself() {
    // It used to be refused, with a message telling the author to write a
    // `RawPtr` and a length — which threw away what the signature knew.
    assert!(codes(&extern_param("[I32]")).is_empty());
}

#[test]
fn a_callback_parameter_is_refused() {
    assert_eq!(codes(&extern_param("(I32) -> I32")), vec!["KSEM182"]);
}

#[test]
fn a_generic_type_in_the_signature_is_judged_by_what_it_resolves_to() {
    // `Opt` is undeclared here, so what this reports is that — not a refusal
    // for being generic. The shape used to be turned away before anything
    // asked what it resolved to, which meant an instantiation that *could*
    // cross was refused for the wrong reason.
    let codes = codes(&extern_param("Opt<I32>"));
    assert!(codes.iter().any(|code| code == "KSEM050"), "{codes:?}");
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

// ----- C-layout aggregates by value ---------------------------------------

#[test]
fn a_c_layout_struct_crosses_by_value_as_an_aggregate() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct Rect { var x: Float\n var y: Float\n var w: Float\n var h: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(r: Rect) -> Rect;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    let row = &program.foreign[0];

    // Both positions name the one table row, and both carry the Kira struct the
    // value is marshalled from and back into.
    let aggregate = row.signature.result().aggregate().expect("an aggregate");
    assert_eq!(row.signature.parameters()[0].aggregate(), Some(aggregate));
    assert!(row.param_wrappers[0].is_some());
    assert_eq!(row.param_wrappers[0], row.result_wrapper);

    // Four `Float` members — the `MetalCGRect` shape, an AArch64 HFA.
    assert_eq!(program.foreign_aggregates.len(), 1);
    let entry = program
        .foreign_aggregates
        .get(aggregate)
        .expect("the table row");
    assert_eq!(
        entry.members(),
        &[kira_runtime_abi::ForeignMember::Scalar(ForeignType::F64); 4]
    );
}

#[test]
fn a_nested_c_layout_struct_is_one_table_row_below_its_container() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct Origin { var x: Float\n var y: Float }\n\
                @FFI.Struct { layout: c; }\n\
                struct Frame { var origin: Origin\n var w: Float\n var h: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(fr: Frame) -> I32;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    let outer = program.foreign[0].signature.parameters()[0]
        .aggregate()
        .expect("an aggregate");
    // The nested member was pushed first, so it holds a lower id — the
    // invariant that makes layout a forward pass.
    let entry = program.foreign_aggregates.get(outer).expect("the row");
    let [kira_runtime_abi::ForeignMember::Aggregate(inner), ..] = entry.members() else {
        panic!("the first member is the nested aggregate: {entry:?}");
    };
    assert!(inner.0 < outer.0);
    assert_eq!(program.foreign_aggregates.len(), 2);
}

#[test]
fn naming_one_aggregate_twice_adds_one_table_row() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct P { var x: Float\n var y: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: a; abi: c; } function f(p: P) -> P;\n\
                @FFI.Extern { library: l; symbol: b; abi: c; } function g(p: P) -> I32;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign_aggregates.len(), 1);
}

#[test]
fn an_ffi_array_member_crosses_as_one_inline_array_row() {
    let text = "@FFI.Array { element: I32; count: 4; }\n\
                struct Cells {}\n\
                @FFI.Struct { layout: c; }\n\
                struct Grid { var cells: Cells\n var weight: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(g: Grid) -> Grid;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    let outer = program.foreign[0].signature.parameters()[0]
        .aggregate()
        .expect("an aggregate");
    let entry = program.foreign_aggregates.get(outer).expect("the row");
    // The array typedef is its own row, held by the struct as a member — the
    // two spellings have the same C layout, and this one keeps the extent.
    let [kira_runtime_abi::ForeignMember::Aggregate(cells), ..] = entry.members() else {
        panic!("the first member is the array row: {entry:?}");
    };
    assert!(cells.0 < outer.0);
    assert_eq!(
        program
            .foreign_aggregates
            .get(*cells)
            .expect("the array row")
            .members(),
        &[kira_runtime_abi::ForeignMember::Array {
            element: kira_runtime_abi::ForeignArrayElement::Scalar(ForeignType::I32),
            count: 4,
        }]
    );
}

#[test]
fn an_ffi_array_holds_its_elements_in_a_named_field() {
    let text = "@FFI.Array { element: I32; count: 3; }\n\
                struct Cells {}\n\
                @Main function main() { let c = Cells { elements: [1, 2] }\n \
                print(c.elements[1]) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_array_fills_a_pointer_to_its_element_type() {
    // `T const *items` beside an `itemCount` is what every descriptor-driven
    // graphics API asks for, and a C array is its elements end to end — so the
    // array typedef's own image is what the pointer addresses. The pointer
    // member is written as an `@FFI.Pointer`, not a bare `RawPtr`, because that
    // is what a generated binding writes.
    let text = "@FFI.Struct { layout: c; }\n\
                struct Item { var location: I32\n var offset: U64 }\n\
                @FFI.Pointer { target: Item; ownership: borrowed; }\n\
                struct ItemPtr {}\n\
                @FFI.Array { element: Item; count: 4; }\n\
                struct Items4 {}\n\
                @FFI.Struct { layout: c; }\n\
                struct List { var items: ItemPtr\n var count: I32 }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } \
                function f(items: ItemPtr, count: I32) -> I32;\n\
                @Main function main() {\n\
                let one = Item { location: 1, offset: 2 }\n\
                print(f(Items4 { elements: [one] }, 1))\n\
                print(f(one, 1))\n\
                let list = List { items: Items4 { elements: [one] }, count: 1 }\n\
                print(list.count)\n\
                return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn an_ffi_array_of_the_wrong_element_does_not_fill_a_pointer() {
    // The extent is not what makes the fill legal — the element type is. An
    // array of something else laid out at that pointer would hand C bytes it
    // reads as the type it declared.
    let text = "@FFI.Struct { layout: c; }\n\
                struct Item { var location: I32 }\n\
                @FFI.Pointer { target: Item; ownership: borrowed; }\n\
                struct ItemPtr {}\n\
                @FFI.Array { element: I32; count: 4; }\n\
                struct Cells {}\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(items: ItemPtr) -> I32;\n\
                @Main function main() { print(f(Cells { elements: [1] })) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

#[test]
fn an_ffi_array_on_its_own_at_the_seam_is_refused_because_c_decays_it() {
    // C turns an array parameter into a pointer, which is a different type with
    // different ownership, so the seam refuses it rather than choosing one.
    let text = "@FFI.Array { element: I32; count: 4; }\n\
                struct Cells {}\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(c: Cells) -> I32;";
    assert_eq!(codes(text), vec!["KSEM187"]);
}

#[test]
fn an_ffi_array_without_an_element_or_a_positive_count_is_refused() {
    let missing = "@FFI.Array { element: I32; }\n\
                   struct Cells {}\n\
                   @Main function main() { return }";
    assert_eq!(codes(missing), vec!["KSEM243"]);
    let empty = "@FFI.Array { element: I32; count: 0; }\n\
                 struct Cells {}\n\
                 @Main function main() { return }";
    assert_eq!(codes(empty), vec!["KSEM243"]);
}

#[test]
fn indexing_an_ffi_array_type_points_at_its_elements_field() {
    let text = "@FFI.Array { element: I32; count: 3; }\n\
                struct Cells {}\n\
                @Main function main() { let c = Cells { elements: [1] }\n \
                print(c[0]) return }";
    assert_eq!(codes(text), vec!["KSEM244"]);
}

#[test]
fn a_kira_function_named_where_a_callback_is_expected_records_one_entry() {
    let text = "@FFI.Callback { abi: c; params: [I32, I32]; result: I32; }\n\
                struct Adder {}\n\
                function combine(a: I32, b: I32) -> I32 { return a + b }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function callAdder(add: Adder, a: I32, b: I32) -> I32;\n\
                @Main function main() { print(callAdder(combine, 1, 2)) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    assert_eq!(program.foreign_callbacks.len(), 1);
    let entry = &program.foreign_callbacks[0];
    assert_eq!(
        entry.signature().parameters(),
        &[
            ForeignTypeSpec::Scalar(ForeignType::I32),
            ForeignTypeSpec::Scalar(ForeignType::I32)
        ]
    );
}

#[test]
fn naming_the_same_function_twice_records_one_callback_entry() {
    let text = "@FFI.Callback { abi: c; params: [I32]; result: Void; }\n\
                struct Sink {}\n\
                function take(x: I32) -> Void { return }\n\
                @FFI.Extern { library: l; symbol: a; abi: c; }\n\
                function first(s: Sink) -> Void;\n\
                @FFI.Extern { library: l; symbol: b; abi: c; }\n\
                function second(s: Sink) -> Void;\n\
                @Main function main() { first(take)\n second(take) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign_callbacks.len(), 1);
}

#[test]
fn a_function_whose_signature_does_not_fit_the_callback_is_refused() {
    let wrong_result = "@FFI.Callback { abi: c; params: [I32]; result: I32; }\n\
                        struct Adder {}\n\
                        function takes(x: I32) -> Void { return }\n\
                        @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                        function use_it(a: Adder) -> Void;\n\
                        @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_result), vec!["KSEM246"]);

    let wrong_arity = "@FFI.Callback { abi: c; params: [I32]; result: Void; }\n\
                       struct Sink {}\n\
                       function takes(x: I32, y: I32) -> Void { return }\n\
                       @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                       function use_it(a: Sink) -> Void;\n\
                       @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_arity), vec!["KSEM246"]);

    // A bare `Int` has no C width, so it is not a callback parameter either.
    let bare_int = "@FFI.Callback { abi: c; params: [I32]; result: Void; }\n\
                    struct Sink {}\n\
                    function takes(x: Int) -> Void { return }\n\
                    @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                    function use_it(a: Sink) -> Void;\n\
                    @Main function main() { use_it(takes) return }";
    assert_eq!(codes(bare_int), vec!["KSEM246"]);
}

#[test]
fn a_callback_declaring_a_type_the_seam_cannot_carry_is_refused_where_it_is_filled() {
    // Declaring it is clean: a generated binding declares every callback its
    // headers name, and most are never filled.
    let declared = "@FFI.Callback { abi: c; params: [[I32]]; result: Void; }\n\
                    struct Sink {}\n\
                    @Main function main() { return }";
    assert!(
        diagnostics(declared).is_empty(),
        "{:?}",
        diagnostics(declared)
    );

    // Handing a Kira function to one is where it cannot work, and is reported.
    let filled = "@FFI.Callback { abi: c; params: [[I32]]; result: Void; }\n\
                  struct Sink {}\n\
                  function takes(x: [I32]) -> Void { return }\n\
                  @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                  function use_it(a: Sink) -> Void;\n\
                  @Main function main() { use_it(takes) return }";
    assert_eq!(codes(filled), vec!["KSEM245"]);
}

/// A callback parameter C passes by value is recorded as the aggregate it is,
/// and the Kira function receives a pointer to it.
///
/// `WGPURequestAdapterCallback` is why: its `WGPUStringView` parameter is fixed
/// by Dawn's header, and `wgpuInstanceRequestAdapter` is the only route to an
/// adapter — so a binding that could not fill this callback could not reach a
/// device at all.
#[test]
fn a_struct_callback_parameter_is_an_aggregate_the_function_takes_by_pointer() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct View { let length: U64 }\n\
                @FFI.Pointer { target: View; ownership: borrowed; }\n\
                struct ViewPtr {}\n\
                @FFI.Callback { abi: c; params: [I32, View]; result: Void; }\n\
                struct Sink {}\n\
                function takes(tag: I32, view: ViewPtr) -> Void { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function use_it(a: Sink) -> Void;\n\
                @Main function main() { use_it(takes) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    let program = program(text);
    assert_eq!(program.foreign_callbacks.len(), 1);
    let declared = program.foreign_callbacks[0].signature().parameters();
    assert_eq!(declared[0], ForeignTypeSpec::Scalar(ForeignType::I32));
    assert!(
        declared[1].aggregate().is_some(),
        "the struct position stays an aggregate on the wire: {declared:?}"
    );
}

/// The struct itself is not what such a function receives, and saying so is the
/// diagnostic — a copy would be a second image of storage C already owns.
#[test]
fn a_struct_callback_parameter_taken_by_value_in_kira_is_refused() {
    let by_value = "@FFI.Struct { layout: c; }\n\
                    struct View { let length: U64 }\n\
                    @FFI.Callback { abi: c; params: [View]; result: Void; }\n\
                    struct Sink {}\n\
                    function takes(view: View) -> Void { return }\n\
                    @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                    function use_it(a: Sink) -> Void;\n\
                    @Main function main() { use_it(takes) return }";
    assert_eq!(codes(by_value), vec!["KSEM246"]);

    // And a pointer to a *different* C-layout struct is a mistake the seam can
    // see, rather than a pointer word it waves through.
    let wrong_target = "@FFI.Struct { layout: c; }\n\
                        struct View { let length: U64 }\n\
                        @FFI.Struct { layout: c; }\n\
                        struct Other { let n: I32 }\n\
                        @FFI.Pointer { target: Other; ownership: borrowed; }\n\
                        struct OtherPtr {}\n\
                        @FFI.Callback { abi: c; params: [View]; result: Void; }\n\
                        struct Sink {}\n\
                        function takes(view: OtherPtr) -> Void { return }\n\
                        @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                        function use_it(a: Sink) -> Void;\n\
                        @Main function main() { use_it(takes) return }";
    assert_eq!(codes(wrong_target), vec!["KSEM246"]);
}

/// A callback *returning* a struct stays refused, and stays refused at the fill
/// site rather than at the declaration.
///
/// Not the same question as a parameter. A parameter is storage C already owns,
/// and its address is the whole answer; a result would have to be C-layout bytes
/// built out of a Kira value, which nothing on this seam carries back.
#[test]
fn a_struct_callback_result_is_refused_where_it_is_filled() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct View { let length: U64 }\n\
                @FFI.Callback { abi: c; params: []; result: View; }\n\
                struct Sink {}\n\
                function gives() -> View { return View {} }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function use_it(a: Sink) -> Void;\n\
                @Main function main() { use_it(gives) return }";
    assert_eq!(codes(text), vec!["KSEM245"]);
}

/// A `String` callback parameter is the one Kira type that *does* fit a C
/// position: it carries the `const char*` C hands over, copied by the thunk.
#[test]
fn a_string_callback_parameter_carries_a_c_string() {
    let text = "@FFI.Callback { abi: c; params: [CString]; result: Void; }\n\
                struct Sink {}\n\
                function takes(x: String) -> Void { print(x) return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function use_it(a: Sink) -> Void;\n\
                @Main function main() { use_it(takes) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

/// The other direction has no answer: C would be handed a pointer somebody has
/// to free, and a Kira `String` belongs to Kira.
#[test]
fn a_string_callback_result_is_refused() {
    let text = "@FFI.Callback { abi: c; params: []; result: CString; }\n\
                struct Sink {}\n\
                function gives() -> String { return \"x\" }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function use_it(a: Sink) -> Void;\n\
                @Main function main() { use_it(gives) return }";
    assert_eq!(codes(text), vec!["KSEM245"]);
}

#[test]
fn a_local_wins_over_a_function_of_the_same_name_in_a_callback_slot() {
    // A callback the program got from C, held in a variable named like a
    // function, is read as the variable.
    let text = "@FFI.Callback { abi: c; params: [I32]; result: Void; }\n\
                struct Sink {}\n\
                function handler(x: I32) -> Void { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; }\n\
                function use_it(a: Sink) -> Void;\n\
                @Main function main() { let handler = Sink {}\n use_it(handler) return }";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert!(
        program(text).foreign_callbacks.is_empty(),
        "the local is the value, so no entry is recorded"
    );
}

#[test]
fn a_c_layout_struct_with_an_unseamable_field_is_refused_by_field_name() {
    let text = "@FFI.Struct { layout: c; }\n\
                struct Bad { var x: Float\n var label: String }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(b: Bad) -> I32;";
    assert_eq!(codes(text), vec!["KSEM182"]);
    let message = &diagnostics(text)[0].message;
    assert!(message.contains("label"), "names the field: {message}");
}

#[test]
fn a_c_layout_struct_may_hold_the_bare_scalars() {
    // A member's offset is decided by its width, and both bare scalars have
    // one: `Int` is `int64_t` and `Float` is `double`. They were refused here
    // while `I64`/`F64` existed to say the width out loud.
    let text = "@FFI.Struct { layout: c; }\n\
                struct Fine { var a: Float\n var n: Int }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(b: Fine) -> I32;";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_multi_field_struct_without_the_annotation_is_still_refused() {
    // The annotation is the author's statement that this type mirrors a C
    // declaration. Without it, adding a Kira field would silently change what
    // the C function receives, so the plain struct keeps its refusal.
    let text = "struct Loose { var x: Float\n var y: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(p: Loose) -> I32;";
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
                @FFI.Extern { library: l; symbol: s; abi: c; } function f(h: Handle) -> I32;";
    assert_eq!(codes(text), Vec::<String>::new());
}

#[test]
fn a_cstring_result_is_accepted_and_is_a_string_in_kira() {
    // The callee returns a `const char*` it keeps and the seam copies the bytes,
    // so the Kira side of the call is an ordinary owned `String` — which is what
    // makes the result assignable to one and printable.
    let text = "@Main function main() { let s: String = f() print(s) return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f() -> CString;";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
}

#[test]
fn a_string_result_is_still_refused_at_the_seam() {
    // `String` is Kira's spelling, not C's: naming it at the seam says nothing
    // about the C type, which is why `CString` is the one that crosses.
    let text = "@Main function main() { return }\n\
                @FFI.Extern { library: l; symbol: s; abi: c; } function f() -> String;";
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
    assert!(
        codes(text).iter().any(|code| code == "KSEM184"),
        "{:?}",
        codes(text)
    );
}

#[test]
fn two_foreign_functions_may_not_share_a_name() {
    let text = "@FFI.Extern { library: l; symbol: s; abi: c; } function dup() -> I32;\n\
                @FFI.Extern { library: l; symbol: t; abi: c; } function dup() -> I32;\n\
                @Main function main() { return }";
    assert!(
        codes(text).iter().any(|code| code == "KSEM185"),
        "{:?}",
        codes(text)
    );
}

/// A payload-less enum crosses as its case's number, which is what a C enum is.
#[test]
fn a_payload_less_enum_is_a_foreign_parameter() {
    let source = r#"
enum Usage { Vertex Index Uniform }

@FFI.Extern { library: fixture; symbol: stride; abi: c; }
function stride(usage: Usage): I32;

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

@FFI.Extern { library: fixture; symbol: take; abi: c; }
function take(reading: Reading): I32;
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
@FFI.Extern { library: fixture; symbol: sum; abi: c; }
function sum(values: [F32], count: I32): F32;

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
@FFI.Extern { library: fixture; symbol: give; abi: c; }
function give(): [F32];
"#;
    let codes = library_codes(source);
    assert!(codes.iter().any(|code| code == "KSEM182"), "{codes:?}");
}

/// An array of something C has no width for is refused by its element.
#[test]
fn an_array_of_a_non_seam_element_is_refused() {
    let source = r#"
@FFI.Extern { library: fixture; symbol: take; abi: c; }
function take(values: [String]): I32;
"#;
    assert!(
        library_codes(source).iter().any(|code| code == "KSEM182"),
        "{:?}",
        library_codes(source)
    );
}

/// A generic instantiation is judged by what it instantiated to.
///
/// It used to be refused for *being* generic, before anything asked what it
/// resolved to. The refusal now names the type and the real reason — this one is
/// a tagged union — so an instantiation that does cross is not turned away for
/// the wrong cause.
#[test]
fn a_generic_instantiation_is_refused_by_what_it_resolves_to() {
    let source = r#"
enum Wrapped<T> { Ok(T) Bad }

@FFI.Extern { library: fixture; symbol: take; abi: c; }
function take(w: Wrapped<I32>): I32;
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
