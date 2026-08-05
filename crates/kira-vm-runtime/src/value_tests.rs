//! Tests for the runtime value model and the object heap with affine drop
//! accounting, split out of `value.rs` on the file-size ladder. They stay a
//! `#[cfg(test)]` submodule beside the code they test.

use super::*;
use kira_runtime_abi::NativeResult;

#[test]
fn alloc_free_balances_and_reuses_slots() {
    let mut heap = Heap::new();
    let a = heap.alloc("one".to_owned());
    let b = heap.alloc("two".to_owned());
    assert_eq!(heap.stats().current, 2);
    heap.free(a);
    assert_eq!(heap.stats().current, 1);
    // Freed slot is reused, so the id index recycles.
    let c = heap.alloc("three".to_owned());
    assert_eq!(heap.get(c), "three");
    assert_eq!(heap.get(b), "two");
    assert_eq!(heap.stats().allocated, 3);
    assert_eq!(heap.stats().freed, 1);
}

#[test]
fn copy_of_a_string_is_independent() {
    let mut heap = Heap::new();
    let a = heap.alloc("x".to_owned());
    let copy = heap.copy_value(Value::Str(a));
    assert_eq!(heap.stats().current, 2);
    heap.drop_value(Value::Str(a));
    // The copy survives the original's drop.
    assert_eq!(heap.stats().current, 1);
    assert!(matches!(copy, Value::Str(_)));
}

#[test]
fn float_formatting_drops_trailing_zero() {
    let mut heap = Heap::new();
    assert_eq!(
        heap.format_and_consume(Value::Float(2.0)).as_deref(),
        Some("2")
    );
    assert_eq!(
        heap.format_and_consume(Value::Float(3.5)).as_deref(),
        Some("3.5")
    );
    assert_eq!(
        heap.format_and_consume(Value::Int(-7)).as_deref(),
        Some("-7")
    );
    assert_eq!(
        heap.format_and_consume(Value::Bool(true)).as_deref(),
        Some("true")
    );
}

#[test]
fn a_struct_has_no_invented_rendering_and_crosses_as_a_tree() {
    let mut heap = Heap::new();
    let value = Value::Struct(heap.alloc_struct(vec![Value::Int(1)]));
    // A struct crosses the seam as a copy of its contents; `lift` leaves the
    // original for its owner to drop.
    assert!(matches!(heap.lift(value), Some(NativeResult::Aggregate(_))));
    // It still has no rendering, and refusing to render it consumes it.
    assert_eq!(heap.format_and_consume(value), None);
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn freeing_a_struct_frees_its_fields() {
    let mut heap = Heap::new();
    let text = heap.alloc("label".to_owned());
    let inner = heap.alloc_struct(vec![Value::Str(text)]);
    let outer = heap.alloc_struct(vec![Value::Struct(inner), Value::Int(7)]);
    assert_eq!(heap.stats().current, 3);
    heap.drop_value(Value::Struct(outer));
    // The string, the inner struct, and the outer struct all go.
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn copying_a_struct_is_deep_so_writes_do_not_alias() {
    let mut heap = Heap::new();
    let text = heap.alloc("a".to_owned());
    let original = heap.alloc_struct(vec![Value::Str(text)]);
    let Value::Struct(copy) = heap.copy_value(Value::Struct(original)) else {
        panic!("a struct copies to a struct");
    };
    assert_ne!(original, copy, "a copy is its own object");

    // Overwrite the copy's string; the original must not see it.
    let replacement = heap.alloc("b".to_owned());
    assert!(heap.set_field(copy, 0, Value::Str(replacement)));
    let Some(Value::Str(original_text)) = heap.field(original, 0) else {
        panic!("the original still holds its string");
    };
    assert_eq!(heap.get(original_text), "a");

    heap.drop_value(Value::Struct(original));
    heap.drop_value(Value::Struct(copy));
    assert_eq!(heap.stats().current, 0, "no field is freed twice or leaked");
}

#[test]
fn overwriting_a_field_drops_what_was_there() {
    let mut heap = Heap::new();
    let text = heap.alloc("gone".to_owned());
    let id = heap.alloc_struct(vec![Value::Str(text)]);
    assert_eq!(heap.stats().current, 2);
    assert!(heap.set_field(id, 0, Value::Int(1)));
    // The replaced string is freed, not leaked: only the struct is live.
    assert_eq!(heap.stats().current, 1);
    heap.drop_value(Value::Struct(id));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn freeing_an_array_frees_its_elements() {
    let mut heap = Heap::new();
    let text = heap.alloc("label".to_owned());
    let inner = heap.alloc_array(vec![Value::Str(text)]);
    let outer = heap.alloc_array(vec![Value::Array(inner), Value::Int(7)]);
    assert_eq!(heap.stats().current, 3);
    heap.drop_value(Value::Array(outer));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn copying_an_array_is_deep_so_writes_do_not_alias() {
    let mut heap = Heap::new();
    let text = heap.alloc("a".to_owned());
    let original = heap.alloc_array(vec![Value::Str(text)]);
    let Value::Array(copy) = heap.copy_value(Value::Array(original)) else {
        panic!("an array copies to an array");
    };
    assert_ne!(original, copy, "a copy is its own object");

    let replacement = heap.alloc("b".to_owned());
    assert!(heap.set_element(copy, 0, Value::Str(replacement)));
    let Some(Value::Str(original_text)) = heap.element(original, 0) else {
        panic!("the original still holds its string");
    };
    assert_eq!(heap.get(original_text), "a");

    heap.drop_value(Value::Array(original));
    heap.drop_value(Value::Array(copy));
    assert_eq!(
        heap.stats().current,
        0,
        "no element is freed twice or leaked"
    );
}

/// The question the array design turned on: copying a struct that holds an
/// array must copy the array, not share the handle. It falls out of the
/// recursion — the struct arm copies each field — but it is the behaviour
/// the whole ownership story rests on, so it is pinned directly rather than
/// inferred from the code.
#[test]
fn copying_a_struct_deep_copies_an_array_field() {
    let mut heap = Heap::new();
    let values = heap.alloc_array(vec![Value::Int(1), Value::Int(2)]);
    let original = heap.alloc_struct(vec![Value::Array(values)]);

    let Value::Struct(copy) = heap.copy_value(Value::Struct(original)) else {
        panic!("a struct copies to a struct");
    };
    let Some(Value::Array(copied_values)) = heap.field(copy, 0) else {
        panic!("the copy holds an array");
    };
    assert_ne!(
        values, copied_values,
        "the copy's array is its own object, not a shared handle"
    );

    // Mutating the copy's array must leave the original's alone.
    assert!(heap.set_element(copied_values, 0, Value::Int(99)));
    assert_eq!(heap.element(values, 0), Some(Value::Int(1)));
    assert_eq!(heap.element(copied_values, 0), Some(Value::Int(99)));

    // …and growing it must not grow the original's either.
    assert!(heap.push_element(copied_values, Value::Int(3)));
    assert_eq!(heap.array_len(values), Some(2));
    assert_eq!(heap.array_len(copied_values), Some(3));

    heap.drop_value(Value::Struct(original));
    heap.drop_value(Value::Struct(copy));
    assert_eq!(heap.stats().current, 0);
}

/// A copy shares its elements, and the sharing is what a free has to account
/// for: the first handle to go releases nothing, the last releases everything.
///
/// Getting this wrong is a double free of every string the array holds, which
/// the heap counters see as `current` going below where it started.
#[test]
fn a_copy_shares_its_elements_and_the_last_holder_frees_them() {
    let mut heap = Heap::new();
    let text = heap.alloc("shared".to_owned());
    let original = heap.alloc_array(vec![Value::Str(text)]);
    let Value::Array(copy) = heap.copy_value(Value::Array(original)) else {
        panic!("an array copies to an array");
    };
    // A slot each, and one string between them — not two.
    assert_eq!(heap.stats().current, 3, "the copy allocated no element");

    heap.drop_value(Value::Array(original));
    let Some(Value::Str(still_there)) = heap.element(copy, 0) else {
        panic!("the copy still holds its string");
    };
    assert_eq!(
        heap.get(still_there),
        "shared",
        "the string outlived one hold"
    );
    assert_eq!(heap.stats().current, 2);

    heap.drop_value(Value::Array(copy));
    assert_eq!(heap.stats().current, 0, "the last holder released it");
}

/// A write through a shared element takes the elements over *before* reading
/// the handle it writes into. Reading first would hand back the object the
/// other array holds, and the write would land in both.
#[test]
fn writing_a_nested_array_of_a_shared_copy_leaves_the_original_alone() {
    let mut heap = Heap::new();
    let inner = heap.alloc_array(vec![Value::Int(1)]);
    let outer = heap.alloc_array(vec![Value::Array(inner)]);
    let Value::Array(copy) = heap.copy_value(Value::Array(outer)) else {
        panic!("an array copies to an array");
    };

    // The copy's own inner array, reached the way a place walk reaches it.
    heap.make_array_unique(copy);
    let Some(Value::Array(copied_inner)) = heap.element(copy, 0) else {
        panic!("the copy holds an array");
    };
    assert_ne!(inner, copied_inner, "taking the elements over copied them");
    assert!(heap.set_element(copied_inner, 0, Value::Int(99)));
    assert_eq!(heap.element(inner, 0), Some(Value::Int(1)));
    assert_eq!(heap.element(copied_inner, 0), Some(Value::Int(99)));

    heap.drop_value(Value::Array(outer));
    heap.drop_value(Value::Array(copy));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn overwriting_an_element_drops_what_was_there() {
    let mut heap = Heap::new();
    let text = heap.alloc("gone".to_owned());
    let id = heap.alloc_array(vec![Value::Str(text)]);
    assert_eq!(heap.stats().current, 2);
    assert!(heap.set_element(id, 0, Value::Int(1)));
    // The replaced string is freed, not leaked: only the array is live.
    assert_eq!(heap.stats().current, 1);
    heap.drop_value(Value::Array(id));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn appending_grows_the_array_in_place() {
    let mut heap = Heap::new();
    let id = heap.alloc_array(Vec::new());
    assert_eq!(heap.array_len(id), Some(0));
    assert!(heap.push_element(id, Value::Int(1)));
    assert!(heap.push_element(id, Value::Int(2)));
    assert_eq!(heap.array_len(id), Some(2));
    assert_eq!(heap.element(id, 1), Some(Value::Int(2)));
    heap.drop_value(Value::Array(id));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn an_array_has_no_invented_rendering_and_crosses_as_a_tree() {
    let mut heap = Heap::new();
    let value = Value::Array(heap.alloc_array(vec![Value::Int(1)]));
    assert!(matches!(heap.lift(value), Some(NativeResult::Aggregate(_))));
    // Refusing to render one still consumes it, so it does not leak.
    assert_eq!(heap.format_and_consume(value), None);
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn copying_an_enum_holds_it_and_both_drops_balance() {
    // An enum with a string payload. A copy is a second hold on the one
    // object — nothing reads an enum in a way that could tell the difference —
    // so dropping the original leaves the copy valid and the payload goes with
    // the last hold, which is what keeps `current == 0` provable.
    let mut heap = Heap::new();
    let text = heap.alloc("payload".to_owned());
    let original = heap.alloc_enum(3, Some(Value::Str(text)));
    assert_eq!(heap.stats().current, 2, "the enum object and its string");

    let Value::Enum(copy) = heap.copy_value(Value::Enum(original)) else {
        panic!("an enum copies to an enum");
    };
    assert_eq!(original, copy, "a copy is the same object, held twice");
    assert_eq!(heap.enum_tag(copy), Some(3), "the tag is carried");
    assert_eq!(heap.stats().current, 2, "the copy allocated nothing");

    heap.drop_value(Value::Enum(original));
    assert_eq!(heap.stats().current, 2, "the copy and its string survive");
    // A payload read is owned, so it outlives the object it came from — which
    // is what a `match` arm's binding relies on.
    let Some(Value::Str(bound)) = heap.enum_payload(copy) else {
        panic!("the variant carries a payload");
    };
    heap.drop_value(Value::Enum(copy));
    assert_eq!(heap.get(bound), "payload", "the binding outlived its enum");
    heap.drop_value(Value::Str(bound));
    assert_eq!(
        heap.stats().current,
        0,
        "nothing leaked, nothing double-freed"
    );
}

#[test]
fn a_payload_less_enum_balances_and_carries_its_tag() {
    let mut heap = Heap::new();
    let value = Value::Enum(heap.alloc_enum(1, None));
    assert_eq!(heap.stats().current, 1);
    // A payload-less enum crosses as its variant tag alone: no tree, nothing
    // allocated on either side.
    assert_eq!(heap.lift(value), Some(NativeResult::Enum(1)));
    // It still has no pinned rendering, like a struct.
    assert_eq!(heap.format_and_consume(value), None);
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn a_handle_of_the_wrong_kind_reads_empty_rather_than_panicking() {
    let mut heap = Heap::new();
    let text = heap.alloc("x".to_owned());
    // A struct handle over a string slot: the VM must not panic on it.
    assert_eq!(heap.fields(StructId(text.0)), &[]);
    assert_eq!(heap.field(StructId(text.0), 0), None);
    assert!(!heap.set_field(StructId(text.0), 0, Value::Int(1)));
    // …and the string is untouched by any of it.
    assert_eq!(heap.get(text), "x");
}

#[test]
fn a_cell_copy_shares_storage_and_a_write_is_visible_through_both() {
    // The one place value semantics stop, and the reason the type exists.
    let mut heap = Heap::new();
    let cell = heap.alloc_cell(Value::Int(1));
    let Value::Cell(shared) = heap.copy_value(Value::Cell(cell)) else {
        panic!("a cell copies to a cell");
    };
    assert_eq!(shared, cell, "a copy is the same box, held twice");
    assert_eq!(heap.stats().current, 1, "and no second box was allocated");

    assert!(heap.cell_set(cell, Value::Int(42)));
    assert_eq!(heap.cell_get(shared), Some(Value::Int(42)));

    heap.drop_value(Value::Cell(cell));
    // The box outlives the first release, because the second hold reads it.
    assert_eq!(heap.cell_get(shared), Some(Value::Int(42)));
    heap.drop_value(Value::Cell(shared));
    assert_eq!(heap.stats().current, 0, "the last hold reclaimed the box");
}

#[test]
fn a_cell_write_releases_the_payload_it_replaced() {
    // The accounting `cell_set` owes: the string that was there goes with the
    // write, and the one that replaced it goes with the box.
    let mut heap = Heap::new();
    let first = heap.alloc("first".to_owned());
    let cell = heap.alloc_cell(Value::Str(first));
    assert_eq!(heap.stats().current, 2, "the string and its box");

    let second = heap.alloc("second".to_owned());
    assert!(heap.cell_set(cell, Value::Str(second)));
    assert_eq!(
        heap.stats().current,
        2,
        "the replaced string was released, not leaked"
    );

    heap.drop_value(Value::Cell(cell));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn a_cell_read_is_owned_and_outlives_the_write_that_replaced_it() {
    // A borrowing read would hand back storage the next write frees.
    let mut heap = Heap::new();
    let held = heap.alloc("held".to_owned());
    let cell = heap.alloc_cell(Value::Str(held));

    let Some(read) = heap.cell_get(cell) else {
        panic!("a cell reads what it holds");
    };
    let Value::Str(read_id) = read else {
        panic!("the payload is a string");
    };
    let replaced = heap.alloc("replaced".to_owned());
    assert!(heap.cell_set(cell, Value::Str(replaced)));
    assert_eq!(heap.get(read_id), "held", "the read survived the write");

    heap.drop_value(read);
    heap.drop_value(Value::Cell(cell));
    assert_eq!(heap.stats().current, 0);
}

#[test]
fn a_cell_operation_on_a_handle_of_the_wrong_kind_refuses_rather_than_panicking() {
    let mut heap = Heap::new();
    let text = heap.alloc("x".to_owned());
    // A cell handle over a string slot: the VM must not panic on it.
    assert_eq!(heap.cell_get(CellId(text.0)), None);
    assert!(!heap.cell_set(CellId(text.0), Value::Int(1)));
    heap.free_cell(CellId(text.0));
    // …and the string is untouched by any of it.
    assert_eq!(heap.get(text), "x");
    heap.drop_value(Value::Str(text));
    assert_eq!(heap.stats().current, 0);
}
