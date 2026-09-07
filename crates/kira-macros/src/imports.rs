//! The module paths one file imports, read before anything is parsed.
//!
//! Expansion runs between lexing and parsing, so the import table the analyzer
//! builds does not exist yet — and a file's imports are what decide which
//! macros it may use, exactly as they decide which names it may write. The
//! paths are read straight off the token stream, which is all an import is:
//! the `import` keyword, a dotted path, and an optional `as` alias that binds
//! a namespace root this question never asks about.

use kira_syntax_model::TokenKind;

use crate::tokens::Lexed;

/// Every module path this file imports, in the order they were written.
///
/// Duplicates are kept: what the caller does with the list is decide package
/// visibility, and one path written twice names one module either way.
pub(crate) fn written(file: &Lexed<'_>) -> Vec<String> {
    let mut paths = Vec::new();
    for index in 0..file.len() {
        if file.kind(index) != TokenKind::Import {
            continue;
        }
        // An `import` that is not the first token of its line is not an import
        // statement: the word can appear inside a macro template, and a
        // template is not the file importing anything.
        if index != 0 && !file.newline_before(index) {
            continue;
        }
        if let Some(path) = dotted_path(file, index + 1) {
            paths.push(path);
        }
    }
    paths
}

/// The dotted path starting at `index`, or `None` when no name is there.
fn dotted_path(file: &Lexed<'_>, index: usize) -> Option<String> {
    if !file.is_ident(index) {
        return None;
    }
    let mut path = file.text_at(index).to_owned();
    let mut cursor = index + 1;
    while file.kind(cursor) == TokenKind::Dot && file.is_ident(cursor + 1) {
        path.push('.');
        path.push_str(file.text_at(cursor + 1));
        cursor += 2;
    }
    Some(path)
}
