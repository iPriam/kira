//! Compiles the generated tree-sitter parser into this crate.

fn main() {
    let src = std::path::Path::new("src");
    let mut build = cc::Build::new();
    build.include(src).file(src.join("parser.c"));
    // The generated parser is C the tree-sitter CLI emitted, not code this
    // repository lints; its unused-parameter warnings are the generator's.
    build.warnings(false);
    build.compile("tree-sitter-kira");
    println!("cargo:rerun-if-changed=src/parser.c");
    println!("cargo:rerun-if-changed=src/tree_sitter");
}
