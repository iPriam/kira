//! The whole supported `@FFI.Extern` surface against the C fixture, and how the
//! library carrying it is declared and linked.

use super::*;

#[test]
fn every_backend_agrees_on_the_ffi_fixture_and_shares_one_counter() {
    let entry = write_ffi_package(include_str!("../../fixtures/ffi/ffi_program.kira"));

    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&entry, backend)))
        .collect();

    for (backend, run) in &runs {
        let stdout = String::from_utf8_lossy(&run.stdout);
        assert_eq!(
            stdout,
            EXPECTED,
            "the {backend} backend produced unexpected FFI output\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            run.status.code(),
            Some(0),
            "the {backend} backend did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }

    // The counter lines are the single-copy proof: whichever way the two foreign
    // calls were routed, they counted 1 then 2 rather than 1 then 1.
    for (backend, run) in &runs {
        let stdout = String::from_utf8_lossy(&run.stdout);
        let tail: Vec<&str> = stdout.lines().rev().take(2).collect();
        assert_eq!(
            tail,
            ["2", "1"],
            "the {backend} backend's counter did not advance 1 then 2",
        );
    }

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}

/// The same program, the same C archive, the same three backends — but the
/// library is declared inline in `package.kira` and there is no
/// `NativeLibs/*.toml` anywhere.
///
/// This is the corpus's own spelling (kira-graphics declares sokol this way and
/// ships no matching TOML), and until it resolved, no real app could link a
/// single `@FFI.Extern`. Byte-identical output to the file-declared run is the
/// statement: where a library is written changes nothing about what it does.
#[test]
fn a_library_declared_inline_in_the_package_links_on_every_backend() {
    let entry = write_inline_ffi_package(
        include_str!("../../fixtures/ffi/ffi_program.kira"),
        &format!(", systemLibs: [\"{HOST_SYSTEM_LIB}\"]"),
    );

    for backend in BACKENDS {
        let run = run_on(&entry, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            EXPECTED,
            "the {backend} backend disagreed on the inline-declared FFI package\nstderr: {}",
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

/// A declared linker flag actually reaches the linker driver.
///
/// The positive test above cannot show this: `-lm` links whether or not it
/// arrives, so a row whose attributes were silently dropped would pass it. A
/// flag naming a library that does not exist can only be observed by failing
/// the link — so a clean exit here means the declaration never made it to the
/// command line.
#[test]
fn a_declared_linker_flag_reaches_the_link_line() {
    const ABSENT: &str = "kira_no_such_system_library";
    let entry = write_inline_ffi_package(
        include_str!("../../fixtures/ffi/ffi_program.kira"),
        &format!(", linkerFlags: [\"-l{ABSENT}\"]"),
    );

    let run = run_on(&entry, "llvm");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert_ne!(
        run.status.code(),
        Some(0),
        "the link succeeded, so the declared linker flag never reached the driver\n\
         stdout: {}",
        String::from_utf8_lossy(&run.stdout),
    );
    assert!(
        stderr.contains(ABSENT),
        "the link failed for some other reason than the declared flag\nstderr: {stderr}",
    );

    let _ = std::fs::remove_dir_all(entry.parent().expect("package directory"));
}
