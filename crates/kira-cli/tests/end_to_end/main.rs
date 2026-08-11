//! End-to-end tests driving the built `kira` binary over real `.kira` files.
//!
//! These exercise the whole pipeline — lexer, parser, salsa analysis, IR,
//! bytecode, VM — plus diagnostic rendering and process exit codes, the way a
//! user invokes it.
//!
//! Split by what a case drives rather than by size: `programs` runs single-file
//! source, `modules` spreads one program over several files, `packages` puts a
//! `package.kira` above it, `exports` builds the `@Export` surface a Rust
//! consumer depends on, `natives` pins what `@Native` does inside a library, and
//! `codegen_units` builds a program big enough for the native backend to split
//! across several of them.
//! Everything shared — writing a temp source, invoking the binary, building a
//! package directory — lives here so a module owns only its own subject.

use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

mod codegen_units;
mod debug;
mod dependencies;
mod exports;
mod ffi;
mod ffi_wasm;
mod foundation;
mod inspect;
mod installed_toolchain;
mod lint_verb;
mod migration;
mod modules;
mod natives;
mod packages;
mod programs;
mod scaffold;
mod tests_verb;
mod web;

/// Writes `source` to a uniquely-named temp `.kira` file and returns its path.
fn write_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let path = std::env::temp_dir().join(format!("kira_e2e_{pid}_{unique}.kira"));
    std::fs::write(&path, source).expect("write temp source");
    path
}

/// Writes a source whose build directory is private to the test.
///
/// Most single-file tests can share the system-temp build directory because
/// they only inspect their child process. Stress tests that emit native code
/// and then clean artifacts need a private parent, though: removing one test's
/// `.kira-build` must not race an LLVM worker belonging to another test.
fn write_isolated_source(source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kira_e2e_isolated_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("isolated temp dir");
    let path = directory.join("main.kira");
    std::fs::write(&path, source).expect("write isolated temp source");
    path
}

/// Runs the real `kira` binary with `args`.
fn kira(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kira"))
        .args(args)
        .output()
        .expect("run kira")
}

/// Runs one source string through `kira run`.
fn run_source(source: &str) -> std::process::Output {
    let path = write_source(source);
    let output = kira(&["run", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    output
}

/// Runs one source string through `kira check`.
fn check_source(source: &str) -> std::process::Output {
    let path = write_source(source);
    let output = kira(&["check", path.to_str().unwrap()]);
    let _ = std::fs::remove_file(&path);
    output
}

/// Writes an entry program plus the modules it imports into one directory, and
/// returns the entry path. A dotted module name is a directory path.
fn write_program(entry: &str, modules: &[(&str, &str)]) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kira_e2e_program_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    for (name, text) in modules {
        let module = directory.join(format!("{name}.kira"));
        if let Some(parent) = module.parent() {
            std::fs::create_dir_all(parent).expect("module directory");
        }
        std::fs::write(&module, text).expect("write module");
    }
    let path = directory.join("main.kira");
    std::fs::write(&path, entry).expect("write entry");
    path
}

/// Writes a package directory with a `package.kira` and one source file, and
/// returns the source path.
fn write_package(kind: &str, source: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let directory = std::env::temp_dir().join(format!("kira_e2e_pkg_{pid}_{unique}"));
    std::fs::create_dir_all(&directory).expect("temp dir");
    std::fs::write(
        directory.join("package.kira"),
        format!(
            "Package uifoundation {{\n    let version = \"0.1.0\"\n    let kind = {kind}\n}}\n"
        ),
    )
    .expect("write package.kira");
    let path = directory.join("uifoundation.kira");
    std::fs::write(&path, source).expect("write source");
    path
}

/// A library with no entrypoint: the thing that could not be written before.
const LIBRARY_SOURCE: &str = "function add(a: Int, b: Int) -> Int { return a + b }\n\
     function greeting(name: String) -> String { return \"hello \" + name }";
