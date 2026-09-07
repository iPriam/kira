//! Seam scalars: the exact C width and value each Kira type crosses as, and the
//! Kira-side names — a handle struct, a `distinct`, an enum — that ride one.

use super::*;

/// A single-scalar-field C handle struct crosses the seam as its field: the
/// result of `ffi_make_handle` (a `struct { unsigned int id; }` by value) is
/// rebuilt into the Kira `Handle`, and `ffi_handle_id` reads the field back out
/// of one. The round trip must produce the same `7 / 8 / 8` on every backend —
/// the LLVM/native and hybrid runs each actually call the C function whose ABI
/// prototype is the struct, through an adapter that names its single `U32`.
#[test]
fn every_backend_agrees_on_a_handle_struct_round_trip() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_handle.kira"));

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "7\n8\n8\n",
            "the {backend} backend disagreed on the handle-struct round trip\nstderr: {}",
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

/// A `distinct` type crosses the seam as the scalar it is, on every backend.
///
/// The C function is `unsigned int ffi_id_u32(unsigned int)`, declared twice in
/// one program: once with `TabId` parameter and result, once with `U32`. Both
/// declarations bind the same symbol through the same `(u32) -> u32` wire
/// signature, so the two calls must print the same number — which is the ABI
/// transparency claim made where it can actually fail, in a real call to a real
/// C symbol on the native and hybrid engines rather than in an assertion about
/// a type table.
#[test]
fn every_backend_agrees_on_a_distinct_type_crossing_the_seam() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_distinct.kira"));

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "4000000000\n4000000000\ntrue\n",
            "the {backend} backend disagreed on a distinct type at the C seam\nstderr: {}",
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

/// A Kira enum and an array named directly in a foreign signature.
///
/// The enum crosses as its case's number, which is what a C enum is; the array
/// crosses as a pointer to elements the seam writes out in C's widths.
#[test]
fn every_backend_agrees_on_enums_and_arrays_named_in_a_signature() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_named_shapes.kira"
    ));

    // The three strides the C enum switches on, the float sum, and the struct.
    const EXPECTED_NAMED_SHAPES: &str = "24\n4\n16\n6.75\n42\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_NAMED_SHAPES,
            "the {backend} backend disagreed on a named shape\nstderr: {}",
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

/// The C `_Bool` seam, in both directions, on every backend.
///
/// `Bool` is the one seam scalar whose C type has fewer valid values than its
/// storage: `_Bool` is a byte, and only `0` and `1` are inside the type. Two
/// engines can therefore disagree about a byte while both compile and both run,
/// which is silent corruption rather than a refusal — the `odd` member here is
/// a byte of `2`, and reading the low bit of it answers `false` where reading
/// the byte answers `true`.
///
/// `boolByte` closes the other direction by reading the parameter object C
/// received, so an argument that crossed as anything but `0` or `1` is a wrong
/// number in this output rather than a flag that happens to still be true.
#[test]
fn every_backend_agrees_on_the_c_bool_seam() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_bool.kira"));

    // `!true`, `!false`; the bytes `1` and `0` that actually crossed; a struct
    // whose `odd` byte is 2 read by value and through a pointer, both `true`
    // with `tag` 7; and 1 + 0*256 + 3*65536 for the struct Kira handed back.
    const EXPECTED_BOOL: &str = "false
true
1
0
true
true
7
true
true
7
196609
";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_BOOL,
            "the {backend} backend disagreed on the C `_Bool` seam\nstderr: {}",
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
