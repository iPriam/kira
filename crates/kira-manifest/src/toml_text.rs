//! Small, typed accessors shared by the legacy TOML manifest reader/writer.
//!
//! These helpers keep the parser's schema decisions separate from TOML's
//! representation details and give callers one escaping implementation for
//! generated basic strings.

use toml::{Table, Value};

/// Returns a child table when `key` names one.
pub fn table<'a>(root: &'a Table, key: &str) -> Option<&'a Table> {
    root.get(key).and_then(Value::as_table)
}

/// Returns a string field when it has the expected TOML type.
pub fn string<'a>(root: &'a Table, key: &str) -> Option<&'a str> {
    root.get(key).and_then(Value::as_str)
}

/// Reads a string array, returning a field-specific message for a wrong TOML
/// shape. An absent key is an empty array.
pub fn strings(root: &Table, key: &str) -> Result<Vec<String>, String> {
    let Some(value) = root.get(key) else {
        return Ok(Vec::new());
    };
    let Some(values) = value.as_array() else {
        return Err(format!("`{key}` must be an array of strings"));
    };
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("`{key}` must be an array of strings"))
        })
        .collect()
}

/// Escapes a value as a TOML basic string, including control characters that
/// would otherwise make the generated document invalid.
pub fn quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\0' => out.push_str("\\u0000"),
            _ => out.push(ch),
        }
    }
    out.push('"');
    out
}

/// Renders a TOML array of strings in a stable inline spelling.
pub fn quoted_array(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| quoted(value))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn string_accessors_reject_wrong_shapes() {
        let root: Table = toml::from_str("name = 42\nvalues = [\"a\", 2]").expect("TOML");
        assert_eq!(string(&root, "name"), None);
        assert!(strings(&root, "values").is_err());
        assert!(strings(&root, "missing").unwrap().is_empty());
    }

    #[test]
    fn generated_strings_escape_toml_controls() {
        assert_eq!(quoted("a\\b\"c\n"), "\"a\\\\b\\\"c\\n\"");
        assert_eq!(
            quoted_array(&["a".to_owned(), "b".to_owned()]),
            "[\"a\", \"b\"]"
        );
    }
}
