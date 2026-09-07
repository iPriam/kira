//! Function pointers across the seam in both directions: one C hands out, and
//! one Kira defines for C to enter.

use super::*;

/// A C function pointer crosses as a member and on its own, on every backend.
///
/// Three cases in one program: a zero-filled `@FFI.Callback` member is NULL and
/// C sees it as null; a pointer C handed out survives a round trip through a
/// Kira struct and is called back through it; and the same pointer passed on its
/// own reaches C as the pointer a function pointer is.
#[test]
fn every_backend_agrees_on_a_c_function_pointer() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program_callback.kira"));

    // A null member (-1), (2 + 5) * 3, and 20 + 22.
    const EXPECTED_CALLBACK: &str = "-1\n21\n42\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_CALLBACK,
            "the {backend} backend disagreed on a C function pointer\nstderr: {}",
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

/// C calls a **Kira** function through a `@FFI.Callback`, on every backend.
///
/// The direction the seam was missing: the pointer is the address of a generated
/// entry thunk, and what it enters differs per backend. The VM's thunk reaches
/// the interpreter through the direct Libffi host, while a native build calls
/// the compiled function directly. C cannot tell, which is the point, so all
/// three must print the same numbers.
///
/// Three shapes: an immediate call back, two crossings in one call (so argument
/// order and a result feeding the next call are both observable), and a callback
/// C stores in a descriptor struct and calls after the call that gave it has
/// returned.
#[test]
fn every_backend_agrees_on_a_kira_function_called_from_c() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_kira_callback.kira"
    ));

    // combine(3, 4) = 34; combine(combine(3, 4), 4) = 344; combine(5, 6) * 2.
    // Then the C-string callback: "kira" echoed and measured (4 * 100 + 7), and
    // a NULL argument arriving as the empty string (0 * 100 + 9).
    const EXPECTED_KIRA_CALLBACK: &str = "34\n344\n112\nkira!\n407\n!\n9\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_KIRA_CALLBACK,
            "the {backend} backend disagreed on a Kira function called from C\nstderr: {}",
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

/// C calls a Kira function with a **struct by value**, on every backend.
///
/// The direction Dawn forces. `wgpuInstanceRequestAdapter` is the only route to
/// an adapter, it answers through a callback, and that callback's third
/// parameter is a `WGPUStringView` by value — a field type fixed by the header,
/// with no userdata word and no sibling entry point to route around it.
///
/// Kira classifies none of it, exactly as it does not for a by-value argument:
/// the generated C entry takes the struct by value, so the target's own C
/// compiler decides how it arrives, and hands the entry thunk its address. The
/// Kira function declares the matching `@FFI.Pointer` and reads members through
/// it.
///
/// The two shapes are the ones a guessed classification gets wrong — a pointer
/// beside a length (`WGPUStringView` itself) and four doubles (an AArch64
/// homogeneous float aggregate in `v0`-`v3`) — with a scalar beside each so
/// argument order is observable, in both orders. The third line is the
/// asynchronous shape: C keeps the callback and enters it after the call that
/// gave it returned.
#[test]
fn every_backend_agrees_on_a_kira_callback_entered_with_a_struct() {
    let entry = write_ffi_package(include_str!(
        "../../fixtures/ffi/ffi_program_struct_callback.kira"
    ));

    // 7 * 100 + 4; 3 * 100 + 0 through a NULL data pointer; 5 * 100 + 4 from
    // the stored callback; then a + d and b + c of the same four doubles.
    const EXPECTED_STRUCT_CALLBACK: &str = "704\n300\n504\n6.25\n5.75\n";

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED_STRUCT_CALLBACK,
            "the {backend} backend disagreed on a struct C passes to a Kira callback\nstderr: {}",
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
