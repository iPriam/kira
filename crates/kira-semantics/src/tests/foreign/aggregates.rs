//! C-layout structs and inline arrays at the seam: the table rows they mint
//! and the members they may hold.

use super::*;

// ----- C-layout aggregates by value ---------------------------------------

#[test]
fn a_c_layout_struct_crosses_by_value_as_an_aggregate() {
    let text = "@FFI.Struct { layout: c }\n\
                struct Rect { var x: Float\n var y: Float\n var w: Float\n var h: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(r: Rect) -> Rect";
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
    let text = "@FFI.Struct { layout: c }\n\
                struct Origin { var x: Float\n var y: Float }\n\
                @FFI.Struct { layout: c }\n\
                struct Frame { var origin: Origin\n var w: Float\n var h: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(fr: Frame) -> I32";
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
    let text = "@FFI.Struct { layout: c }\n\
                struct P { var x: Float\n var y: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: a, abi: c } function f(p: P) -> P\n\
                @FFI.Extern { library: l, symbol: b, abi: c } function g(p: P) -> I32";
    assert!(diagnostics(text).is_empty(), "{:?}", diagnostics(text));
    assert_eq!(program(text).foreign_aggregates.len(), 1);
}

#[test]
fn an_ffi_array_member_crosses_as_one_inline_array_row() {
    let text = "@FFI.Array { element: I32, count: 4 }\n\
                struct Cells {}\n\
                @FFI.Struct { layout: c }\n\
                struct Grid { var cells: Cells\n var weight: Float }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(g: Grid) -> Grid";
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
    let text = "@FFI.Array { element: I32, count: 3 }\n\
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
    let text = "@FFI.Struct { layout: c }\n\
                struct Item { var location: I32\n var offset: U64 }\n\
                @FFI.Pointer { target: Item, ownership: borrowed }\n\
                struct ItemPtr {}\n\
                @FFI.Array { element: Item, count: 4 }\n\
                struct Items4 {}\n\
                @FFI.Struct { layout: c }\n\
                struct List { var items: ItemPtr\n var count: I32 }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } \
                function f(items: ItemPtr, count: I32) -> I32\n\
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
    let text = "@FFI.Struct { layout: c }\n\
                struct Item { var location: I32 }\n\
                @FFI.Pointer { target: Item, ownership: borrowed }\n\
                struct ItemPtr {}\n\
                @FFI.Array { element: I32, count: 4 }\n\
                struct Cells {}\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(items: ItemPtr) -> I32\n\
                @Main function main() { print(f(Cells { elements: [1] })) return }";
    assert_eq!(codes(text), vec!["KSEM183"]);
}

#[test]
fn an_ffi_array_on_its_own_at_the_seam_is_refused_because_c_decays_it() {
    // C turns an array parameter into a pointer, which is a different type with
    // different ownership, so the seam refuses it rather than choosing one.
    let text = "@FFI.Array { element: I32, count: 4 }\n\
                struct Cells {}\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(c: Cells) -> I32";
    assert_eq!(codes(text), vec!["KSEM187"]);
}

#[test]
fn an_ffi_array_without_an_element_or_a_positive_count_is_refused() {
    let missing = "@FFI.Array { element: I32 }\n\
                   struct Cells {}\n\
                   @Main function main() { return }";
    assert_eq!(codes(missing), vec!["KSEM243"]);
    let empty = "@FFI.Array { element: I32, count: 0 }\n\
                 struct Cells {}\n\
                 @Main function main() { return }";
    assert_eq!(codes(empty), vec!["KSEM243"]);
}

#[test]
fn indexing_an_ffi_array_type_points_at_its_elements_field() {
    let text = "@FFI.Array { element: I32, count: 3 }\n\
                struct Cells {}\n\
                @Main function main() { let c = Cells { elements: [1] }\n \
                print(c[0]) return }";
    assert_eq!(codes(text), vec!["KSEM244"]);
}

#[test]
fn a_c_layout_struct_with_an_unseamable_field_is_refused_by_field_name() {
    let text = "@FFI.Struct { layout: c }\n\
                struct Bad { var x: Float\n var label: String }\n\
                @Main function main() { return }\n\
                @FFI.Extern { library: l, symbol: s, abi: c } function f(b: Bad) -> I32";
    assert_eq!(codes(text), vec!["KSEM182"]);
    let message = &diagnostics(text)[0].message;
    assert!(message.contains("label"), "names the field: {message}");
}
