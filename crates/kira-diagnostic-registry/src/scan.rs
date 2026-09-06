//! Finds every diagnostic code the toolchain names in its own source.
//!
//! A code reaches a user as a string literal: `Code::known("KSEM107")` in the
//! compiler, `code: "KLINT003"` in Foundation's lint runner. So the scan reads
//! string literals and keeps the ones spelled like a code, which is what makes
//! "the table lists what the compiler emits" a claim a test can check rather
//! than a claim a reader has to trust.
//!
//! Test code is not a source of codes. A fixture may name `KLINT099` to prove
//! that a code outside the catalog still renders, and cataloging it would make
//! the registry claim the compiler emits it. The scan therefore skips
//! `#[cfg(test)]` items and any path whose file or directory name contains
//! `test`, which is how this workspace spells a test-only module.
//!
//! A generated artifact is not a source of codes either. `DiagnosticCodes.kira`
//! names every code in the table because the table wrote it, and counting it
//! would make the drift check answer itself.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::{RegistryError, artifacts};

/// Where every code the toolchain names was found, keyed by the code.
pub type Emitted = BTreeMap<String, PathBuf>;

/// Every code named by compiler source under `crates/` or by Foundation.
pub fn emitted_codes(repo: &Path) -> Result<Emitted, RegistryError> {
    let generated: Vec<PathBuf> = artifacts()
        .into_iter()
        .map(|artifact| repo.join(artifact.path))
        .collect();
    let mut found = Emitted::new();
    collect(
        &repo.join("crates"),
        "rs",
        Syntax::Rust,
        repo,
        &generated,
        &mut found,
    )?;
    collect(
        &repo.join("foundation"),
        "kira",
        Syntax::Kira,
        repo,
        &generated,
        &mut found,
    )?;
    Ok(found)
}

/// How a file's string literals are spelled.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Syntax {
    /// Rust, with raw strings, char literals, and `#[cfg(test)]` items.
    Rust,
    /// Kira, with `//` comments and backslash escapes.
    Kira,
}

/// Whether a path component names test-only code.
fn is_test_component(name: &str) -> bool {
    name.contains("test")
}

/// Walks `root` for files with `extension`, adding what they name to `found`.
fn collect(
    root: &Path,
    extension: &str,
    syntax: Syntax,
    repo: &Path,
    generated: &[PathBuf],
    found: &mut Emitted,
) -> Result<(), RegistryError> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory).map_err(|error| RegistryError::Unreadable {
            path: directory.clone(),
            reason: error.to_string(),
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| RegistryError::Unreadable {
                path: directory.clone(),
                reason: error.to_string(),
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if path.is_dir() {
                if name != "target" && !is_test_component(&name) {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|found| found == extension)
                && !is_test_component(&name)
                && !generated.contains(&path)
            {
                files.push(path);
            }
        }
    }
    files.sort();
    for path in files {
        let text = fs::read_to_string(&path).map_err(|error| RegistryError::Unreadable {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        let relative = path.strip_prefix(repo).unwrap_or(&path).to_path_buf();
        for code in codes_in(&text, syntax) {
            found.entry(code).or_insert_with(|| relative.clone());
        }
    }
    Ok(())
}

/// Whether `text` is spelled like a diagnostic code: `K`, uppercase letters,
/// then three digits.
fn is_code(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 6 {
        return false;
    }
    let (letters, digits) = bytes.split_at(bytes.len() - 3);
    letters[0] == b'K'
        && letters.iter().all(u8::is_ascii_uppercase)
        && digits.iter().all(u8::is_ascii_digit)
}

/// Every code named by a string literal in `text`.
fn codes_in(text: &str, syntax: Syntax) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut cursor = 0usize;
    let mut literals: Vec<(usize, String)> = Vec::new();
    let mut skipped: Vec<(usize, usize)> = Vec::new();
    while cursor < bytes.len() {
        let rest = &bytes[cursor..];
        if rest.starts_with(b"//") {
            cursor += line_comment(rest);
        } else if syntax == Syntax::Rust && rest.starts_with(b"/*") {
            cursor += block_comment(rest);
        } else if rest.starts_with(b"\"") {
            let (length, value) = quoted(rest);
            literals.push((cursor + 1, value));
            cursor += length;
        } else if syntax == Syntax::Rust && rest.starts_with(b"'") {
            cursor += char_literal(rest);
        } else if syntax == Syntax::Rust && starts_raw_string(text, cursor) {
            let (length, value) = raw_string(rest);
            literals.push((cursor, value));
            cursor += length;
        } else {
            if syntax == Syntax::Rust && rest.starts_with(b"#[cfg(test)]") {
                skipped.push((cursor, cursor + item_length(rest)));
            }
            cursor += 1;
        }
    }
    literals
        .into_iter()
        .filter(|(at, value)| {
            is_code(value) && !skipped.iter().any(|(from, to)| at >= from && at < to)
        })
        .map(|(_, value)| value)
        .collect()
}

/// The length of a `//` comment, including its newline.
fn line_comment(rest: &[u8]) -> usize {
    rest.iter()
        .position(|byte| *byte == b'\n')
        .map_or(rest.len(), |end| end + 1)
}

/// The length of a `/* */` comment, which nests.
fn block_comment(rest: &[u8]) -> usize {
    let mut depth = 0usize;
    let mut cursor = 0usize;
    while cursor < rest.len() {
        if rest[cursor..].starts_with(b"/*") {
            depth += 1;
            cursor += 2;
        } else if rest[cursor..].starts_with(b"*/") {
            depth = depth.saturating_sub(1);
            cursor += 2;
            if depth == 0 {
                return cursor;
            }
        } else {
            cursor += 1;
        }
    }
    rest.len()
}

/// The length and contents of a `"…"` literal, escapes resolved to nothing so
/// an escape can never spell a code.
fn quoted(rest: &[u8]) -> (usize, String) {
    let mut cursor = 1usize;
    let mut value = Vec::new();
    while cursor < rest.len() {
        match rest[cursor] {
            b'"' => return (cursor + 1, String::from_utf8_lossy(&value).into_owned()),
            b'\\' => cursor += 2,
            byte => {
                value.push(byte);
                cursor += 1;
            }
        }
    }
    (rest.len(), String::from_utf8_lossy(&value).into_owned())
}

/// The length of a `'x'` literal, or 1 for a lifetime.
fn char_literal(rest: &[u8]) -> usize {
    if rest.len() > 2 && rest[1] == b'\\' {
        return rest[2..]
            .iter()
            .position(|byte| *byte == b'\'')
            .map_or(rest.len(), |end| end + 3);
    }
    if rest.len() > 2 && rest[2] == b'\'' {
        return 3;
    }
    1
}

/// Whether a raw string starts at `at`, rather than an identifier ending in
/// `r` followed by something else.
fn starts_raw_string(text: &str, at: usize) -> bool {
    let bytes = text.as_bytes();
    if bytes.get(at) != Some(&b'r') {
        return false;
    }
    if at > 0 && (bytes[at - 1].is_ascii_alphanumeric() || bytes[at - 1] == b'_') {
        return false;
    }
    let mut cursor = at + 1;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    bytes.get(cursor) == Some(&b'"')
}

/// The length and contents of an `r#"…"#` literal.
fn raw_string(rest: &[u8]) -> (usize, String) {
    let mut hashes = 0usize;
    let mut cursor = 1usize;
    while rest.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    cursor += 1;
    let start = cursor;
    let mut terminator = Vec::with_capacity(hashes + 1);
    terminator.push(b'"');
    terminator.resize(hashes + 1, b'#');
    while cursor < rest.len() {
        if rest[cursor..].starts_with(&terminator) {
            let value = String::from_utf8_lossy(&rest[start..cursor]).into_owned();
            return (cursor + terminator.len(), value);
        }
        cursor += 1;
    }
    (
        rest.len(),
        String::from_utf8_lossy(&rest[start..]).into_owned(),
    )
}

/// The length of the item an attribute at the start of `rest` decorates.
fn item_length(rest: &[u8]) -> usize {
    let mut cursor = 0usize;
    let mut depth = 0usize;
    let mut opened = false;
    while cursor < rest.len() {
        match rest[cursor] {
            b'{' => {
                depth += 1;
                opened = true;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                if opened && depth == 0 {
                    return cursor + 1;
                }
            }
            b';' if !opened => return cursor + 1,
            _ => {}
        }
        cursor += 1;
    }
    rest.len()
}

#[cfg(test)]
mod tests {
    use super::{Syntax, codes_in, is_code};

    #[test]
    fn a_code_is_a_k_letters_and_three_digits() {
        assert!(is_code("KSEM107"));
        assert!(is_code("KLINT003"));
        assert!(is_code("KIC001"));
        assert!(!is_code("KS001"));
        assert!(!is_code("KSEM10"));
        assert!(!is_code("ksem107"));
        assert!(!is_code("KSEM107 "));
    }

    #[test]
    fn rust_literals_answer_and_comments_do_not() {
        let source = r#"
            // KSEM001 in a comment is not emitted.
            /* nor /* nested */ KSEM002 */
            fn emit() { report(Code::known("KSEM107")); }
        "#;
        assert_eq!(codes_in(source, Syntax::Rust), vec!["KSEM107".to_owned()]);
    }

    #[test]
    fn a_cfg_test_item_is_not_a_source_of_codes() {
        let source = r#"
            fn emit() { report("KSEM107"); }
            #[cfg(test)]
            mod tests {
                fn fixture() { report("KLINT099"); }
            }
        "#;
        assert_eq!(codes_in(source, Syntax::Rust), vec!["KSEM107".to_owned()]);
    }

    #[test]
    fn a_char_literal_does_not_swallow_the_literal_after_it() {
        let source = "fn f() { if byte == '\"' { report(\"KSEM107\") } }";
        assert_eq!(codes_in(source, Syntax::Rust), vec!["KSEM107".to_owned()]);
    }

    #[test]
    fn a_raw_string_is_read_as_one_literal() {
        let source = "const SOURCE: &str = r#\"let a = \"KSEM001\"\"#; fn f() { e(\"KSEM107\") }";
        assert_eq!(codes_in(source, Syntax::Rust), vec!["KSEM107".to_owned()]);
    }

    #[test]
    fn kira_literals_answer() {
        let source = "// KLINT001 in a comment\nDiagnostics.error(m, code: \"KLINT003\")\n";
        assert_eq!(codes_in(source, Syntax::Kira), vec!["KLINT003".to_owned()]);
    }
}
