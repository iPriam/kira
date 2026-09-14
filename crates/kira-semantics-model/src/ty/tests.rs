use super::*;

fn table_with_point() -> (TypeTable, StructId) {
    let mut table = TypeTable::new();
    let id = table
        .structs_mut()
        .declare(StructDef {
            name: "Point".to_owned(),
            fields: vec![FieldDef {
                name: "x".to_owned(),
                ty: Type::INT,
                mutable: true,
            }],
            c_layout: false,
            drop_glue: None,
        })
        .expect("a fresh name declares");
    (table, id)
}

#[test]
fn a_struct_type_names_itself_in_diagnostics() {
    let (table, id) = table_with_point();
    assert_eq!(table.type_name(Type::Struct(id)), "Point");
    assert_eq!(table.type_name(Type::INT), "Int");
}

#[test]
fn a_struct_needs_move_but_does_not_move_on_bind() {
    let (_, id) = table_with_point();
    // The two predicates answer differently for a struct, and that split
    // is the whole point: `f(p)` into an owned param is KSEM108, while
    // `let q = p` still copies.
    assert!(!Type::Struct(id).is_trivially_copyable());
    assert!(!Type::Struct(id).moves_on_bind());
}

#[test]
fn an_array_needs_move_and_also_moves_on_bind() {
    let mut table = TypeTable::new();
    let ints = table.array_of(Type::INT);
    // The one type that answers both questions `no`/`yes`: it needs `move`
    // into an owned parameter *and* `let alias = xs` consumes `xs`.
    assert!(!ints.is_trivially_copyable());
    assert!(ints.moves_on_bind());
}

#[test]
fn a_string_needs_move_and_a_scalar_does_not() {
    assert!(!Type::String.is_trivially_copyable());
    assert!(Type::INT.is_trivially_copyable());
    assert!(Type::FLOAT.is_trivially_copyable());
    assert!(Type::Bool.is_trivially_copyable());
    assert!(Type::Void.is_trivially_copyable());
}

#[test]
fn pointer_words_and_capture_cells_are_trivially_copyable() {
    let (mut table, target) = table_with_point();
    let foreign = table.foreign_ptr_to(target);
    let cell = table.cell_of(Type::String);
    assert!(Type::RawPtr.is_trivially_copyable());
    assert!(foreign.is_trivially_copyable());
    assert!(cell.is_trivially_copyable());
    assert!(!table.owns_heap(foreign));
    assert!(table.owns_heap(cell));
}

#[test]
fn only_an_array_moves_on_bind() {
    let (mut table, id) = table_with_point();
    let ints = table.array_of(Type::INT);
    for ty in [
        Type::INT,
        Type::FLOAT,
        Type::Bool,
        Type::Void,
        Type::Error,
        Type::String,
        Type::Struct(id),
    ] {
        assert!(!ty.moves_on_bind(), "{ty:?} must not move on bind");
    }
    assert!(ints.moves_on_bind(), "an array is the type that does");
}

#[test]
fn an_error_type_never_collects_an_ownership_diagnostic() {
    // A type error already reported must not also produce KSEM108.
    assert!(Type::Error.is_trivially_copyable());
}

#[test]
fn an_array_is_not_printable_because_no_format_is_pinned() {
    let mut table = TypeTable::new();
    let ints = table.array_of(Type::INT);
    // Same evidence as a struct: no corpus call site, no golden file. A
    // separator invented here would be invented language surface.
    assert!(!ints.is_printable());
    assert!(Type::INT.is_printable());
}

#[test]
fn every_type_is_assignable_to_any() {
    let (mut table, point) = table_with_point();
    let ints = table.array_of(Type::INT);
    for ty in [
        Type::INT,
        Type::FLOAT,
        Type::Int(IntSpelling::U8),
        Type::Float(FloatSpelling::F32),
        Type::Bool,
        Type::String,
        Type::RawPtr,
        Type::Struct(point),
        ints,
        Type::Any,
    ] {
        assert!(ty.assignable_to(Type::Any), "{ty:?} must widen into `Any`");
    }
}

#[test]
fn any_is_assignable_to_nothing_but_itself() {
    let (mut table, point) = table_with_point();
    let ints = table.array_of(Type::INT);
    for ty in [
        Type::INT,
        Type::FLOAT,
        Type::Bool,
        Type::String,
        Type::Void,
        Type::RawPtr,
        Type::Struct(point),
        ints,
    ] {
        assert!(
            !Type::Any.assignable_to(ty),
            "`Any` must not narrow to {ty:?} without a recovery form"
        );
    }
    assert!(Type::Any.assignable_to(Type::Any));
    // `Error` stays symmetric so one type error does not cascade.
    assert!(Type::Any.assignable_to(Type::Error));
    assert!(Type::Error.assignable_to(Type::Any));
}

#[test]
fn any_names_itself_and_owns_heap_storage() {
    let table = TypeTable::new();
    assert_eq!(table.type_name(Type::Any), "Any");
    // Whatever it erased: the box is the thing that is owned.
    assert!(table.owns_heap(Type::Any));
}

#[test]
fn any_is_opaque_rather_than_scalar_or_printable() {
    // Nothing may read an erased value, so it is neither a scalar the
    // backends move inline nor something `print` has a format for.
    assert!(!Type::Any.is_scalar());
    assert!(!Type::Any.is_printable());
    assert!(!Type::Any.is_numeric());
    assert!(!Type::Any.is_array());
    // It may own heap storage, so it needs `move` into an owned parameter,
    // and it copies rather than moving on bind.
    assert!(!Type::Any.is_trivially_copyable());
    assert!(!Type::Any.moves_on_bind());
}

#[test]
fn only_an_already_erased_or_failed_type_skips_the_boxing_step() {
    assert!(!Type::Any.erases_into_any());
    assert!(!Type::Error.erases_into_any());
    assert!(Type::INT.erases_into_any());
    assert!(Type::String.erases_into_any());
    assert!(Type::Void.erases_into_any());
}

#[test]
fn the_top_type_resolves_by_name() {
    assert_eq!(Type::from_name("Any"), Some(Type::Any));
    // The spelling is exact: AGENTS.md legislates `Any`, and nothing else
    // resolves to it.
    assert_eq!(Type::from_name("any"), None);
    assert_eq!(Type::from_name("ANY"), None);
}

#[test]
fn interned_array_types_are_equal_and_assignable() {
    let mut table = TypeTable::new();
    let a = table.array_of(Type::INT);
    let b = table.array_of(Type::INT);
    let strings = table.array_of(Type::String);
    assert_eq!(a, b);
    assert!(a.assignable_to(b));
    assert!(
        !a.assignable_to(strings),
        "no widening between element types"
    );
    assert!(a.assignable_to(Type::Error));
    assert!(Type::Error.assignable_to(a));
}
