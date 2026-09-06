//! Pointer words: the null a program spells, members read behind a pointer, and
//! the storage an array argument points at.

use super::*;

/// Members read *through* an `@FFI.Pointer`, on every backend.
///
/// C owns the struct and hands over its address; Kira reads the members behind
/// it with no accessor compiled into a shim. That is what the twenty
/// `kg_event_*` accessors in kira-graphics existed to work around.
///
/// The members are of mixed width and signedness with padding between them, so
/// a read at the wrong offset gives a wrong *answer* rather than a crash — the
/// failure a backend can have silently. `next` is read through and then read
/// through again, which is both the linked-structure shape and the proof that a
/// member's offset is its own rather than its first leaf's.
#[test]
fn every_backend_agrees_on_members_read_through_a_pointer() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_pointer_read.kira"
    ));

    // `kind` is 200, which is only positive read as unsigned; `code` is -1234,
    // which is only negative read as signed; `delta` is -7 from a `signed char`;
    // `weight` is 1.5 widened from `float`; the tail's `kind` is 9; and the
    // first and last touches' `pos_y` are 20.5 and 80.5, which come out right
    // only if the array member's own offset and the element stride are both
    // right.
    const EXPECTED_POINTER_READ: &str = "200
-1234
-7
1.5
9
20.5
80.5
";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_POINTER_READ,
            "the {backend} backend disagreed on a member read through a pointer
stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly
stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// A Kira array handed to C as a pointer and a count, on every backend.
///
/// This is the shape every graphics API takes, and without it a caller streams
/// values into a C-side buffer one at a time through a shim — which is what
/// kira-graphics did before this.
///
/// The widths differ on purpose. Kira holds a `[F32]` as `double`s and C reads
/// four bytes each, so a seam that handed over the array's own storage would
/// give C wrong *numbers* rather than a wrong pointer — the kind of failure that
/// looks like a rendering bug rather than a crash.
#[test]
fn every_backend_agrees_on_an_array_handed_to_c() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_array_argument.kira"
    ));

    // 1.5 + 2.25 + 3.0, then 10 + 20 + 30 + 40, then an empty array — which is
    // a null pointer and a zero count rather than a trap.
    const EXPECTED_ARRAY_ARGUMENT: &str = "6.75\n100\n0\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_ARRAY_ARGUMENT,
            "the {backend} backend disagreed on an array handed to C\nstderr: {}",
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

/// An array argument travels *out* only: C reads a copy, and writes to it are
/// not visible afterwards.
///
/// The seam writes the elements into its own buffer in C's widths, because Kira
/// holds them in Kira's. Handing over the array's own storage is not an option,
/// so a write through the pointer lands somewhere Kira does not read — which is
/// worth pinning, because a caller reaching for the out-parameter shape a C API
/// often uses would otherwise find it silently doing nothing.
#[test]
fn an_array_argument_is_a_copy_out_not_a_shared_buffer() {
    let entry = write_ffi_package(
        r#"
@FFI.Extern { library: ffifixture, symbol: ffi_fill_floats, abi: c }
function ffiFillFloats(values: [F32], count: I32): Void

@Main
function main() {
    var values: [F32] = [1.0, 2.0]
    ffiFillFloats(values, 2)
    print(values[0])
    print(values[1])
    return
}
"#,
    );

    // 1 and 2, not the 99s C wrote into the buffer it was handed.
    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            "1\n2\n",
            "the {backend} backend disagreed on an array argument's direction\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
}

/// `RawPtr.null` written, handed to C, and compared, on every backend.
///
/// The null is the value a C API answers most often and the one a Kira program
/// could not previously spell. Comparison is what makes it useful: a pointer
/// compares as the word it is, so an `@FFI.Pointer` type, a plain `RawPtr`, and
/// a zero-filled member all meet at the same test.
#[test]
fn every_backend_agrees_on_the_null_pointer() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_null.kira"));

    const EXPECTED_NULL: &str = "0
true
true
false
true
0
true
false
true
true
true
5
";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_NULL,
            "the {backend} backend disagreed on the null pointer\nstderr: {}",
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
