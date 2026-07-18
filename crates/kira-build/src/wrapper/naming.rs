//! Kira names to Rust names, and the refusals when a name will not travel.
//!
//! The frontend already snake_cases an export's consumer-facing name and
//! already refuses two exports that collide there, so most of the work is done
//! before this module sees anything. What is left is the part only a *Rust*
//! generator knows: whether the resulting name is spellable in Rust at all.
//!
//! # Why a raw identifier rather than a rename
//!
//! A Kira export called `match` snake_cases to `match`, which is a Rust
//! keyword. Renaming it (`match_`, `r_match`) would make the Rust name differ
//! from the Kira name for a reason the author cannot see from their source, so
//! this emits `r#match` instead: the consumer writes `ui.r#match(..)`, which is
//! ugly and correct, and the name still round-trips.
//!
//! Four keywords cannot be raw identifiers at all — `crate`, `self`, `Self`,
//! `super` — so those are refused by name with the reason, rather than silently
//! renamed.

use crate::wrapper::WrapperError;

/// Rust's keywords, reserved words included, as of the 2024 edition.
///
/// Written out rather than derived: there is no way to ask the compiler, and a
/// list that is one name short produces a generated crate that does not compile
/// — which is a loud failure, but at the consumer's build rather than here.
const KEYWORDS: &[&str] = &[
    "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum", "extern",
    "false", "fn", "for", "gen", "if", "impl", "in", "let", "loop", "match", "mod", "move", "mut",
    "pub", "ref", "return", "self", "static", "struct", "super", "trait", "true", "try", "type",
    "unsafe", "use", "where", "while", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "Self",
];

/// The keywords no `r#` prefix can rescue.
const UNRAWABLE: &[&str] = &["crate", "self", "Self", "super"];

/// Whether `name` is a Rust keyword.
fn is_keyword(name: &str) -> bool {
    KEYWORDS.contains(&name)
}

/// Whether `name` is spellable as a plain Rust identifier, keywords aside.
///
/// ASCII-only on purpose. Rust accepts non-ASCII identifiers, but a symbol name
/// that has to survive a file name, a crate name, and eventually a linker is not
/// the place to find out which layer normalizes Unicode differently.
fn is_ascii_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(first) if first.is_ascii_alphabetic() || first == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// The Rust spelling of a value-position name (a method, a parameter).
///
/// Returns the name unchanged, or raw-escaped when it is a keyword.
pub fn value_ident(kind: &'static str, name: &str) -> Result<String, WrapperError> {
    if !is_ascii_ident(name) {
        return Err(WrapperError::Unspellable {
            kind,
            name: name.to_owned(),
            reason: "it is not an ASCII Rust identifier",
        });
    }
    if UNRAWABLE.contains(&name) {
        return Err(WrapperError::Unspellable {
            kind,
            name: name.to_owned(),
            reason: "it is one of the four Rust keywords that no `r#` prefix can escape",
        });
    }
    if is_keyword(name) {
        return Ok(format!("r#{name}"));
    }
    Ok(name.to_owned())
}

/// The Rust type name for an exported Kira class.
///
/// Kira class names are already UpperCamel by convention, so this validates
/// rather than transforms — a class named `button` stays `button`, because
/// inventing `Button` would make the consumer's type name differ from the one
/// the author wrote for a reason nothing in their source explains.
pub fn type_ident(name: &str) -> Result<String, WrapperError> {
    if !is_ascii_ident(name) {
        return Err(WrapperError::Unspellable {
            kind: "an exported class",
            name: name.to_owned(),
            reason: "it is not an ASCII Rust identifier",
        });
    }
    if UNRAWABLE.contains(&name) || is_keyword(name) {
        return Err(WrapperError::Unspellable {
            kind: "an exported class",
            name: name.to_owned(),
            reason: "a Rust type may not be named after a keyword",
        });
    }
    Ok(name.to_owned())
}

/// The Rust type name for the library itself: `uifoundation` to `Uifoundation`.
///
/// Unlike a class, this name is not something the author wrote in Kira — it is
/// derived from the package name, which is lower-case by manifest convention —
/// so capitalizing it invents nothing.
pub fn library_type_ident(library: &str) -> Result<String, WrapperError> {
    let mut out = String::with_capacity(library.len());
    let mut capitalize = true;
    for c in library.chars() {
        if c == '_' || c == '-' {
            capitalize = true;
            continue;
        }
        if capitalize {
            out.extend(c.to_uppercase());
            capitalize = false;
        } else {
            out.push(c);
        }
    }
    type_ident(&out)
}

/// The crate and module name for a library: a valid lower-case Rust identifier.
pub fn library_ident(library: &str) -> Result<String, WrapperError> {
    let ident = value_ident("a library", library)?;
    if ident.starts_with("r#") {
        return Err(WrapperError::Unspellable {
            kind: "a library",
            name: library.to_owned(),
            reason: "a crate may not be named after a Rust keyword",
        });
    }
    Ok(ident)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ordinary_name_passes_through_unchanged() {
        assert_eq!(
            value_ident("an export", "make_button").unwrap(),
            "make_button"
        );
        assert_eq!(type_ident("Button").unwrap(), "Button");
    }

    #[test]
    fn a_keyword_export_becomes_a_raw_identifier() {
        assert_eq!(value_ident("an export", "match").unwrap(), "r#match");
        assert_eq!(value_ident("an export", "type").unwrap(), "r#type");
    }

    #[test]
    fn the_four_unrawable_keywords_are_refused_by_name() {
        for name in ["crate", "self", "Self", "super"] {
            let error = value_ident("an export", name).expect_err(name);
            assert!(error.to_string().contains(name), "{error}");
        }
    }

    #[test]
    fn a_class_named_after_a_keyword_is_refused_rather_than_escaped() {
        // `r#struct` is a legal type name, but a generated `pub struct r#struct`
        // is a worse answer than saying so.
        assert!(type_ident("struct").is_err());
    }

    #[test]
    fn a_library_name_capitalizes_into_a_type_name() {
        assert_eq!(library_type_ident("uifoundation").unwrap(), "Uifoundation");
        assert_eq!(library_type_ident("ui_foundation").unwrap(), "UiFoundation");
    }

    #[test]
    fn a_library_name_that_is_not_an_identifier_is_refused() {
        assert!(library_ident("ui-foundation").is_err());
        assert!(library_ident("9lives").is_err());
    }
}
