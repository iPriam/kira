//! Differential tests: the VM, the LLVM/native backend, and the hybrid bundle
//! must not disagree.
//!
//! Parity is proven, not asserted. Each case compiles one program through every
//! backend from the same IR and requires identical program output and exit
//! status — that is the whole contract a Kira user sees, so a divergence here
//! is a real bug in one of them.
//!
//! # What each backend does with an annotation
//!
//! `--backend vm` compiles every function to bytecode and `--backend llvm`
//! makes every function native: an execution boundary needs two engines, and
//! these builds have one, so both ignore `@Runtime`/`@Native` entirely. Only
//! `--backend hybrid` splits a program on them. That is what makes these three
//! comparable on *any* program: the annotations change where code runs without
//! changing what it does, and a case here that says otherwise is a bug.
//!
//! So an unannotated case exercises no crossing on any backend, hybrid
//! included — it compiles to a single engine like the other two. The annotated
//! cases are the ones that build a real boundary, and they are still parity
//! tests: agreeing with `vm` and `llvm`, which ignored the annotations, is the
//! statement that a boundary changed where the code ran and nothing else.
//!
//! These only run when `kira` was built with its `llvm` feature; without it
//! there is no native backend to compare against.

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU32, Ordering};

/// Writes `source` to a uniquely-named temp `.kira` file.
///
/// Each program gets its own directory: `.kira-build` artifacts land beside the
/// source, and tests run in parallel.
fn write_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kira_parity_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    let path = directory.join("program.kira");
    std::fs::write(&path, source).expect("write temp source");
    path
}

/// Writes an entry program plus the modules it imports into one directory.
///
/// Returns the entry path. Module names are file stems, and a dotted name is a
/// directory path — the same rule the compiler resolves an `import` by — so a
/// case can write `("Foundation/Web", ...)` for `import Foundation.Web`.
fn write_program(entry: &str, modules: &[(&str, &str)]) -> PathBuf {
    let path = write_source(entry);
    let directory = path.parent().expect("program directory");
    for (name, text) in modules {
        let module = directory.join(format!("{name}.kira"));
        if let Some(parent) = module.parent() {
            std::fs::create_dir_all(parent).expect("module directory");
        }
        std::fs::write(&module, text).expect("write module");
    }
    path
}

/// Asserts every backend agrees on a program shipping files beside it.
///
/// `files` are written verbatim at the paths named, relative to the entry's own
/// directory — a `.ksl` shader keeps its extension, unlike the `.kira` modules
/// [`write_program`] names by stem.
fn assert_parity_with_files(entry: &str, files: &[(&str, &str)]) -> String {
    let path = write_source(entry);
    let directory = path.parent().expect("program directory").to_path_buf();
    for (name, text) in files {
        let file = directory.join(name);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).expect("file directory");
        }
        std::fs::write(&file, text).expect("write file");
    }
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(&directory);

    let (_, reference) = &runs[0];
    let expected = String::from_utf8_lossy(&reference.stdout).into_owned();
    for (backend, run) in &runs[1..] {
        assert_eq!(
            expected,
            String::from_utf8_lossy(&run.stdout),
            "the vm and {backend} backends disagree on output for:\n{entry}\n\
             vm stderr: {}\n{backend} stderr: {}",
            String::from_utf8_lossy(&reference.stderr),
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            reference.status.code(),
            run.status.code(),
            "the vm and {backend} backends disagree on exit code for:\n{entry}\n\
             {backend} stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    expected
}

/// Asserts every backend agrees on a multi-module program.
fn assert_module_parity(entry: &str, modules: &[(&str, &str)]) -> String {
    let path = write_program(entry, modules);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    let (_, reference) = &runs[0];
    let expected = String::from_utf8_lossy(&reference.stdout).into_owned();
    for (backend, run) in &runs[1..] {
        assert_eq!(
            expected,
            String::from_utf8_lossy(&run.stdout),
            "the vm and {backend} backends disagree on output for:\n{entry}\n\
             vm stderr: {}\n{backend} stderr: {}",
            String::from_utf8_lossy(&reference.stderr),
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            reference.status.code(),
            run.status.code(),
            "the vm and {backend} backends disagree on exit code for:\n{entry}\n\
             {backend} stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    expected
}

/// Runs `source` on one backend.
fn run_on(source_path: &std::path::Path, backend: &str) -> Output {
    kira()
        .args(["run", "--backend", backend, source_path.to_str().unwrap()])
        .output()
        .expect("run kira")
}

/// The repo's own `foundation/`, which every run here is pinned to.
///
/// Foundation discovery prefers the toolchain `current.toml` points at over the
/// checkout, so a machine with an older toolchain installed would otherwise run
/// these against *that* Foundation and prove nothing about the one in the tree.
/// Pinning makes the tests hermetic and says which Foundation they are about.
fn foundation_home() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../foundation")
}

/// A `kira` command pinned to this checkout's Foundation.
fn kira() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_kira"));
    command.env("KIRA_FOUNDATION_HOME", foundation_home());
    command
}

/// Every backend a program must behave identically on.
const BACKENDS: [&str; 3] = ["vm", "llvm", "hybrid"];

/// Asserts every backend agrees on a program that touches the filesystem.
///
/// Same contract as [`assert_parity`], with one addition: each run's working
/// directory is the program's own temp directory, so a relative path the
/// program writes lands there and not in whatever directory the test runner was
/// started from. The three runs are sequential and share that directory on
/// purpose — a program here starts by clearing what it is about to build, so
/// each backend sees the same world the one before it did.
fn assert_parity_on_disk(source: &str) -> String {
    let path = write_source(source);
    let directory = path.parent().expect("program directory").to_path_buf();
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| {
            let run = kira()
                .args(["run", "--backend", backend, path.to_str().unwrap()])
                .current_dir(&directory)
                .output()
                .expect("run kira");
            (*backend, run)
        })
        .collect();

    let (_, reference) = &runs[0];
    let expected = String::from_utf8_lossy(&reference.stdout).into_owned();
    for (backend, run) in &runs[1..] {
        assert_eq!(
            expected,
            String::from_utf8_lossy(&run.stdout),
            "the vm and {backend} backends disagree on output for:\n{source}\n\
             vm stderr: {}\n{backend} stderr: {}",
            String::from_utf8_lossy(&reference.stderr),
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            reference.status.code(),
            run.status.code(),
            "the vm and {backend} backends disagree on exit code for:\n{source}\n\
             {backend} stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(&directory);
    expected
}

/// Asserts every backend agrees on `source`, returning the output they produced.
///
/// The VM is the reference: it is the simplest of the three and the one whose
/// semantics the other two are defined to mirror, so a disagreement names the
/// backend that drifted rather than leaving two answers to choose between.
fn assert_parity(source: &str) -> String {
    let path = write_source(source);
    let runs: Vec<(&str, Output)> = BACKENDS
        .iter()
        .map(|backend| (*backend, run_on(&path, backend)))
        .collect();
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));

    let (_, reference) = &runs[0];
    let expected = String::from_utf8_lossy(&reference.stdout).into_owned();

    for (backend, run) in &runs[1..] {
        let stdout = String::from_utf8_lossy(&run.stdout).into_owned();
        assert_eq!(
            expected,
            stdout,
            "the vm and {backend} backends disagree on output for:\n{source}\n\
             vm stderr: {}\n{backend} stderr: {}",
            String::from_utf8_lossy(&reference.stderr),
            String::from_utf8_lossy(&run.stderr),
        );
        assert_eq!(
            reference.status.code(),
            run.status.code(),
            "the vm and {backend} backends disagree on exit code for:\n{source}\n\
             {backend} stderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    expected
}

/// Asserts every backend refuses `source` the same way: no output, non-zero
/// exit.
///
/// A trap is the one case where stdout alone would prove too little — a program
/// that printed nothing and exited cleanly would pass a stdout comparison.
fn assert_trap_parity(source: &str, before_the_trap: &str) {
    let path = write_source(source);
    for backend in BACKENDS {
        let run = run_on(&path, backend);
        assert_eq!(
            String::from_utf8_lossy(&run.stdout),
            before_the_trap,
            "the {backend} backend printed something other than the output \
             preceding the trap for:\n{source}",
        );
        assert_eq!(
            run.status.code(),
            Some(1),
            "the {backend} backend did not trap for:\n{source}\nstderr: {}",
            String::from_utf8_lossy(&run.stderr),
        );
    }
    let _ = std::fs::remove_dir_all(path.parent().expect("program directory"));
}

mod aliases;
mod any;
mod arithmetic;
mod array_sharing;
mod arrays;
mod attempts;
mod bitwise;
mod calls;
mod captured_vars;
mod classes;
mod closures;
mod compiler;
mod construct_requirements;
mod constructs;
mod control_flow;
mod conversions;
mod derives;
mod enums;
mod examples;
mod existentials;
mod ffi;
mod ffi_types;
mod file_system;
mod foundation;
mod generics;
mod imports;
mod libraries;
mod logic;
mod macros;
mod matches;
mod mutation;
mod native_state;
mod ownership;
mod seam;
mod shaders;
mod strings;
mod structs;
mod tasks;
mod widening;
mod widths;
