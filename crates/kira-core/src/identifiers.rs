//! Kira identifier sanitizing helpers used by code generation.

/// Kira reserved words an emitted identifier must not collide with.
const KEYWORDS: &[&str] = &[
    "annotation",
    "capability",
    "class",
    "comptime",
    "macro",
    "quote",
    "construct",
    "trait",
    "enum",
    "struct",
    "type",
    "extends",
    "extend",
    "attempt",
    "try",
    "Self",
    "async",
    "function",
    "generated",
    "override",
    "overridable",
    "targets",
    "uses",
    "let",
    "var",
    "return",
    "import",
    "as",
    "if",
    "else",
    "for",
    "in",
    "while",
    "break",
    "continue",
    "match",
    "true",
    "false",
];

/// Rewrites `name` (or `fallback` when empty) into a valid Kira identifier:
/// non-alphanumeric bytes become `_`, and keywords / leading digits gain a `_` prefix.
pub fn sanitize_kira_identifier(name: &str, fallback: &str) -> String {
    let source = if name.is_empty() { fallback } else { name };
    let mut output = String::with_capacity(source.len() + 1);
    if is_keyword(source) || source.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        output.push('_');
    }
    for byte in source.bytes() {
        output.push(if byte.is_ascii_alphanumeric() || byte == b'_' {
            byte as char
        } else {
            '_'
        });
    }
    output
}

fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_paths_leading_digits_empty_names_and_keywords() {
        let cases = [
            ("hello-world", "hello_world"),
            ("9lives", "_9lives"),
            ("", "Package"),
            ("class", "_class"),
        ];
        for (input, expected) in cases {
            assert_eq!(sanitize_kira_identifier(input, "Package"), expected);
        }
    }
}
