//! Frontend inspection commands, driven through the real `kira` binary.

use crate::{kira, write_package, write_source};

#[test]
fn tokens_print_the_lexer_output_and_eof() {
    let path = write_source("function add(value: Int) -> Int { return value + 1 }");
    let output = kira(&["tokens", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("`function`"), "{stdout}");
    assert!(stdout.contains("\"add\""), "{stdout}");
    assert!(stdout.contains("end of input"), "{stdout}");
}

#[test]
fn ast_prints_nodes_and_resolved_names() {
    let path = write_source("function add(value: Int) -> Int { return value + 1 }");
    let output = kira(&["ast", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Function"), "{stdout}");
    assert!(stdout.contains("# Names"), "{stdout}");
    assert!(stdout.contains("add"), "{stdout}");
}

#[test]
fn doc_renders_declarations_and_doc_comments() {
    let path = write_source(
        "/// Adds two numbers.\nfunction add(a: Int, b: Int) -> Int { return a + b }\n",
    );
    let output = kira(&["doc", path.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_file(&path);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# kira_e2e_"), "{stdout}");
    assert!(stdout.contains("## function `add`"), "{stdout}");
    assert!(stdout.contains("Adds two numbers."), "{stdout}");
    assert!(
        stdout.contains("function add(a: Int, b: Int) -> Int"),
        "{stdout}"
    );
}

#[test]
fn doc_expands_a_library_directory() {
    let source = write_package(
        ".Library",
        "/// Adds two numbers.\nfunction add(a: Int, b: Int) -> Int { return a + b }\n",
    );
    let root = source.parent().expect("package directory").to_path_buf();
    let output = kira(&["doc", root.to_str().expect("a utf-8 path")]);
    let _ = std::fs::remove_dir_all(&root);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("# uifoundation"), "{stdout}");
    assert!(stdout.contains("## function `add`"), "{stdout}");
}

#[test]
fn inspection_requires_a_readable_source_file() {
    let output = kira(&["tokens", "kira-file-that-does-not-exist.kira"]);

    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("cannot read"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
