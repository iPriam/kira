//! Every example in the repo must behave identically on every backend.

use std::path::PathBuf;
use std::process::Command;

use crate::{BACKENDS, run_on};

/// Every example in the repo must behave identically on every backend.
///
/// One directory is one unit of work, and the directories run concurrently:
/// each is an independent program with its own build artifacts, and the work
/// is subprocess-bound, so this test's wall time is the slowest example
/// rather than the sum of all of them. Files *within* a directory stay
/// serial — they share the directory's artifacts.
#[test]
fn every_example_agrees_on_every_backend() {
    let examples = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .expect("the examples directory");

    let directories: Vec<PathBuf> = std::fs::read_dir(&examples)
        .expect("read examples")
        .map(|entry| entry.expect("example entry").path())
        .filter(|path| path.is_dir())
        .collect();

    let checked = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for directory in &directories {
            let checked = &checked;
            scope.spawn(move || {
                checked.fetch_add(
                    check_example_directory(directory),
                    std::sync::atomic::Ordering::Relaxed,
                );
            });
        }
    });
    assert!(
        checked.load(std::sync::atomic::Ordering::Relaxed) > 0,
        "no examples were checked"
    );
}

/// Checks one example directory on every backend, returning how many programs
/// it held.
fn check_example_directory(directory: &std::path::Path) -> usize {
    // A library example is *built*, never run: it has no entry point by
    // construction, so comparing stdout would only ever compare three
    // identical refusals. What parity means for one is that every backend
    // produces an artifact from the same package — checked below, then
    // skipped here.
    //
    // The manifest's declared `kind` is what decides, not its mere
    // existence: an example package that declares an application kind is a
    // program like any other, and must be run rather than quietly held to a
    // library's rules. An unrecognized kind fails by name instead of
    // falling through to whichever branch happens to be next.
    let manifest = directory.join("package.kira");
    if manifest.is_file() {
        let text = std::fs::read_to_string(&manifest).expect("read package.kira");
        if text.contains("kind = .Library") {
            check_library_example(directory);
            return 1;
        }
        assert!(
            text.contains("kind = .App"),
            "example package `{}` declares a kind this test does not classify: {text}",
            directory.display(),
        );
    }
    // An example directory is a *program*, not a bag of files: once one of
    // them declares `@Main` and imports the others, running a module on its
    // own would only prove that a library has no entry point. So a
    // directory with a `main.kira` is entered through it, and every other
    // directory keeps the one-file-is-the-program rule it had.
    let entry = directory.join("main.kira");
    let sources: Vec<PathBuf> = if entry.is_file() {
        vec![entry]
    } else {
        std::fs::read_dir(directory)
            .expect("read example directory")
            .map(|file| file.expect("example file").path())
            .collect()
    };
    let mut checked = 0;
    for source in sources {
        if source.extension().is_none_or(|kind| kind != "kira") {
            continue;
        }
        let vm = run_on(&source, "vm");
        for backend in &BACKENDS[1..] {
            let run = run_on(&source, backend);
            assert_eq!(
                String::from_utf8_lossy(&vm.stdout),
                String::from_utf8_lossy(&run.stdout),
                "example `{}` differs between the vm and {backend} backends.\n\
                 {backend} stderr: {}",
                source.display(),
                String::from_utf8_lossy(&run.stderr),
            );
            assert_eq!(
                vm.status.code(),
                run.status.code(),
                "example `{}` exits differently on the vm and {backend} backends",
                source.display(),
            );
        }
        checked += 1;
    }
    checked
}

/// Asserts every backend builds the library package in `directory`.
///
/// The worked example a reader is pointed at, held to the same bar as the
/// generated fixtures: if the documented `kirac build` stops working on an
/// engine, this fails rather than the README quietly becoming false.
fn check_library_example(directory: &std::path::Path) {
    let sources: Vec<PathBuf> = std::fs::read_dir(directory)
        .expect("read example directory")
        .map(|file| file.expect("example file").path())
        .filter(|path| {
            path.extension().is_some_and(|kind| kind == "kira")
                && path.file_name().is_some_and(|name| name != "package.kira")
        })
        .collect();
    assert_eq!(
        sources.len(),
        1,
        "a library example is one source file beside its manifest: {}",
        directory.display(),
    );
    let source = &sources[0];

    for backend in BACKENDS {
        let run = Command::new(env!("CARGO_BIN_EXE_kirac"))
            .args(["build", "--backend", backend, source.to_str().unwrap()])
            .output()
            .expect("run kirac");
        assert!(
            run.status.success(),
            "the {backend} backend failed to build the library example `{}`:\nstderr: {}",
            source.display(),
            String::from_utf8_lossy(&run.stderr),
        );
    }

    // Running one is refused, which is the other half of what a library is —
    // and the README beside it says so, so it is checked rather than trusted.
    let run = run_on(source, "vm");
    assert_eq!(
        run.status.code(),
        Some(1),
        "the library example ran instead of being refused",
    );
    let _ = std::fs::remove_dir_all(directory.join(".kira-build"));
}
