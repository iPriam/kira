//! C-layout aggregates crossing the seam by value and by address: structs,
//! inline arrays, members that are themselves aggregates, and the storage a
//! `retains:` parameter transfers.

use super::*;

/// C-layout structs cross the seam **by value**, in both directions, and every
/// backend has to agree on every byte.
///
/// The shapes are chosen for the ABI cases that a hand-written classifier gets
/// wrong, and that a `byval`/`sret` lowering cannot express at all:
///
/// - `ffi_quad` is four doubles — on AArch64 a homogeneous float aggregate
///   passed and returned in `v0`-`v3`, not in memory,
/// - `ffi_outer` nests a struct, so the Kira value has to be rebuilt with its
///   nesting rather than flattened,
/// - `ffi_mixed` pads between a `signed char` and a `double`, so a marshaller
///   that packed fields would produce a different number.
///
/// Kira classifies none of it: the generated C shim hands each struct to clang
/// by value, and clang applies the ABI it defines. What this test proves is that
/// the three engines then agree — and, because the values are computed by the C
/// side out of fields Kira wrote, that the bytes arrived where C expected them.
#[test]
fn every_backend_agrees_on_c_layout_structs_by_value() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_aggregate.kira"));

    // rect_sum(1.5, 2.5); rect_scale by 2; quad_sum(1+2+3+4); quad_make's first
    // and last; outer_sum(3+4+5) through a nested struct; outer_make's three
    // fields read back; mixed_sum(2+3+4) across padding; mixed_make's three.
    const EXPECTED_AGGREGATE: &str = "4\n3\n5\n10\n5\n8\n12\n11\n22\n33\n9\n7\n8\n9\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_AGGREGATE,
            "the {backend} backend disagreed on a struct crossing by value\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// Inline `@FFI.Array` members cross by value, in both directions, on every
/// backend.
///
/// The two element shapes are covered: `ffi_grid` holds four `int`s inline
/// beside a `double`, and `ffi_board` holds three C structs inline beside an
/// `int`. Three writes prove the fill rule the two engines have to agree on —
/// a full array, a short one whose remaining C storage stays zero, and a
/// zero-filled construction with no elements at all — and the reads prove the
/// whole declared extent comes back, not a prefix.
#[test]
fn every_backend_agrees_on_inline_c_arrays() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_array.kira"));

    // gridSum full (20), short (11), zero-filled (3); gridMake(7)'s first and
    // last cells, its element count and weight; boardSum (27); boardMake(3)'s
    // third slot `p`, first slot `q`, and tag.
    const EXPECTED_ARRAY: &str = "20\n11\n3\n7\n10\n4\n70\n27\n5\n2\n300\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY,
            "the {backend} backend disagreed on an inline C array\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// A `CString` member and a struct handed to C by address, on every backend.
///
/// The storage question both raise is the same one, and it has one answer: a
/// An ordinary descriptor call borrows its ownership tree and frees it on
/// return. `ffi_desc_keep` declares `retains: d`, so that one call consumes the
/// tree into the retained registry; `ffi_desc_recall` proves C can still read it
/// after the giving call returned.
///
/// The reference implementation crashes on the by-value case here. That is not
/// a behaviour to reproduce: the value is well defined, so this compiler
/// computes it rather than inheriting a segmentation fault.
#[test]
fn every_backend_agrees_on_a_c_string_member_and_a_struct_by_address() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_cstring.kira"));

    // A zero-filled title is NULL (0 * 10 + 7); "kira" by pointer (2 * 10 + 9)
    // and by value (2 * 10 + 8); the empty string is a real pointer, not NULL
    // (1 * 10 + 5); the kept pointer still reads "kira" (2 * 10 + 6); and a
    // retained by-value member still reads it too (2 * 10 + 4); and a retained
    // top-level CString survives as well (2 * 10 + 3).
    const EXPECTED_CSTRING: &str = "7\n29\n28\n15\n26\n24\n23\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CSTRING,
            "the {backend} backend disagreed on a CString member\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

#[test]
fn retained_c_storage_is_counted_and_everything_else_balances() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_cstring.kira"));
    for backend in ["llvm", "hybrid"] {
        let run = run_on_with_heap_report(&entry, backend);
        let report = assert_native_heap_balanced(backend, &run);
        assert_eq!(
            report.retained,
            4,
            "retained pointer, by-value, and CString calls own four blocks on {backend}\n{}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// The same array, named as a C-layout struct's data pointer, on every backend.
///
/// A graphics API rarely takes a pointer and a count as two arguments: it takes
/// a descriptor holding both. `sg_range` is the one every sokol upload goes
/// through, and while an array could only cross at an argument, that descriptor
/// had to be built in a C helper whose whole job was naming the address of an
/// array — two of them survived in kira-graphics for exactly that reason.
///
/// The last line is the one that matters most: C keeps the pointer and reads it
/// after the call returns, so it fails if the elements got storage that dies
/// with the descriptor naming them.
#[test]
fn every_backend_agrees_on_an_array_named_in_a_c_layout_member() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_array_member.kira"
    ));

    // 1.5 + 2.25 + 3.0 by address, 10 + 20 + 30 + 40 by value, an empty array —
    // which is a null pointer the fixture answers -1 for — then 7 + 8 + 9 read
    // back out of a pointer C kept.
    const EXPECTED_ARRAY_MEMBER: &str = "6.75\n100\n-1\n24\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY_MEMBER,
            "the {backend} backend disagreed on an array named in a C-layout member\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// An item list — several C structs behind one pointer — on every backend.
///
/// A pointer position already took the struct it points at, which covers a
/// descriptor with one of something. Every descriptor-driven graphics API asks
/// for several beside a count instead: vertex attributes, bind group entries,
/// colour targets. A C array is its elements laid out end to end, which is what
/// an `@FFI.Array` reserves, so the array type fills the pointer.
///
/// The third line is the one worth watching: a Kira array shorter than the
/// extent zero-fills the rest, and the count beside the pointer is what says
/// how many C reads.
#[test]
fn every_backend_agrees_on_an_item_list_behind_one_pointer() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_item_list.kira"));

    // Three items by argument, one by argument, two of a four-slot extent, and
    // two named inside a descriptor whose member is an `@FFI.Pointer`.
    const EXPECTED_ITEM_LIST: &str = "6040\n4005\n2003\n12014\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ITEM_LIST,
            "the {backend} backend disagreed on an item list behind one pointer\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// An extension chained onto a descriptor, on every backend.
///
/// The pointer member's target is the base link; the value written is a struct
/// that *begins* with one. A struct and its first member share an address, which
/// is what every `nextInChain`/`pNext` cast in an extensible header relies on —
/// and reaching a WebGPU surface from a window has no other shape at all.
#[test]
fn every_backend_agrees_on_an_extension_chained_onto_a_descriptor() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_chained.kira"));

    // 7 with no chain, 7*3 with one link, 7*3*5 with two, and 7 again for a
    // link the walker does not recognise.
    const EXPECTED_CHAINED: &str = "7\n21\n105\n7\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CHAINED,
            "the {backend} backend disagreed on a chained extension\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}
