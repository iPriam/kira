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

/// Every import this file writes, as `(module path, namespace root)`.
///
/// The root is the alias when the import wrote one and the path's last segment
/// otherwise, which is the rule the analyzer's own table uses. Carrying it
/// matters because a root binds once per file: `import Alpha as X` followed by
/// `import Beta as X` leaves only `Beta` bound, and a reading that recorded
/// two roots `Alpha` and `Beta` would call both packages imported and hand a
/// file macros from a binding it no longer has.
///
/// Duplicates are kept: deciding what a repeated root means is the table's
/// job, and it is the same table either way.
///
/// Only at brace depth zero, which is where an import statement can be. A
/// macro template is a brace-delimited body full of ordinary-looking source,
/// and a line reading `import Inner` inside one is text the macro will paste,
/// not the declaring file importing anything. Reading it as an import handed
/// the file that package's macros without the file ever importing it — the
/// visibility hole one level down from the one this scanner exists to close.
pub(crate) fn written(file: &Lexed<'_>) -> Vec<(String, String)> {
    let mut paths = Vec::new();
    let mut depth = 0u32;
    for index in 0..file.len() {
        match file.kind(index) {
            TokenKind::LBrace => {
                depth += 1;
                continue;
            }
            TokenKind::RBrace => {
                depth = depth.saturating_sub(1);
                continue;
            }
            TokenKind::Import => {}
            _ => continue,
        }
        if depth != 0 {
            continue;
        }
        // An `import` that is not the first token of its line is not an import
        // statement either: the word can appear where a name belongs.
        if index != 0 && !file.newline_before(index) {
            continue;
        }
        if let Some((path, next)) = dotted_path(file, index + 1) {
            let root = alias(file, next)
                .unwrap_or_else(|| path.rsplit('.').next().unwrap_or(&path).to_owned());
            paths.push((path, root));
        }
    }
    paths
}

/// The name bound by an `as` immediately at `index`, if one is written.
fn alias(file: &Lexed<'_>, index: usize) -> Option<String> {
    (file.kind(index) == TokenKind::As && file.is_ident(index + 1))
        .then(|| file.text_at(index + 1).to_owned())
}

/// The dotted path starting at `index`, with the token index just past it.
fn dotted_path(file: &Lexed<'_>, index: usize) -> Option<(String, usize)> {
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
    Some((path, cursor))
}
