//! The Web pipeline, end to end: object emission, the emscripten link, and
//! the module actually executing.
//!
//! The build must not merely succeed — the linked module runs under node and
//! its output is compared byte-for-byte with the VM's on the same program,
//! because "it linked" says nothing about what the program will print in a
//! browser. This is the Web leg of the parity contract the backend-parity
//! suite proves for the host engines.
//!
//! These tests require `emcc` and `node` on PATH, exactly as building for the
//! Web does; a machine without them fails here rather than skipping, so a
//! green run always means the Web path was genuinely exercised.

use std::path::Path;
use std::process::Command;

use crate::{kirac, write_source};

/// A program whose output exercises the cases browsers get wrong when a
/// runtime borrows the host's formatting: large floats, wrapping arithmetic,
/// and 64-bit integers.
const PROGRAM: &str = "@Main function main() {\n\
     print(1075 * 3)\n\
     print(-9223372036854775807 - 1)\n\
     print(3.5 / 2.0)\n\
     print(1000000000000000000000.0)\n\
     return\n\
   }";

/// Runs the built module under node, returning its stdout.
fn run_under_node(js: &Path) -> std::process::Output {
    Command::new("node")
        .arg(js)
        .output()
        .expect("run node (required for the Web end-to-end tests)")
}

#[test]
fn a_web_build_links_and_runs_with_the_vms_exact_output() {
    let path = write_source(PROGRAM);

    let vm = kirac(&["run", path.to_str().expect("utf-8 path")]);
    assert!(
        vm.status.success(),
        "{}",
        String::from_utf8_lossy(&vm.stderr)
    );

    let web = kirac(&[
        "build",
        "--device",
        "wasm32",
        path.to_str().expect("utf-8 path"),
    ]);
    assert!(
        web.status.success(),
        "the Web build must link: {}",
        String::from_utf8_lossy(&web.stderr)
    );

    let stem = path.file_stem().expect("stem").to_string_lossy();
    let js = path
        .parent()
        .expect("parent")
        .join(".kira-build")
        .join("web")
        .join(format!("{stem}.js"));
    assert!(js.is_file(), "the link must leave {}", js.display());

    let node = run_under_node(&js);
    let _ = std::fs::remove_file(&path);
    assert!(
        node.status.success(),
        "the module must run to completion under node: {}",
        String::from_utf8_lossy(&node.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&node.stdout),
        String::from_utf8_lossy(&vm.stdout),
        "the Web build and the VM disagree on the same program"
    );
}

#[test]
fn a_wasm64_build_is_refused_by_name() {
    let path = write_source(PROGRAM);
    let output = kirac(&[
        "build",
        "--device",
        "wasm64",
        path.to_str().expect("utf-8 path"),
    ]);
    let _ = std::fs::remove_file(&path);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("wasm64") && stderr.contains("wasm32"),
        "the refusal must name the device and the alternative: {stderr}"
    );
}
