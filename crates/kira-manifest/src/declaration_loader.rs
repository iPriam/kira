//! Loads a [`ProjectManifest`] from a `package.kira` declaration.
//!
//! # Why this is not the Kira frontend
//!
//! The file *looks* like Kira and is deliberately not parsed as Kira. `Package`
//! is not a keyword, a top-level `Package Name { ... }` construct is not an
//! item the grammar has, and `.Library` is not an expression — so parsing this
//! with `kira-parser` would mean inventing language surface for a config file.
//! It is also the wrong layering: `kira-manifest` is layer 5 and the frontend
//! is layers 1-2, and a manifest reader that needed the compiler would put the
//! compiler inside every tool that reads a manifest.
//!
//! So this is a reader for one small, fixed, oracle-pinned shape:
//!
//! ```text
//! Package DemoLibrary {
//!     let version = "0.1.0"
//!     let kind = .Library
//!     let moduleRoot = "DemoLibrary"
//!     let defaults = Defaults { executionMode: .Vm, buildTarget: .Host }
//! }
//! ```
//!
//! Unknown keys are ignored rather than rejected, because this crate's model
//! covers a subset of the fields a manifest may carry and rejecting the rest
//! would make every new field a breaking change. A key this reader *does* know
//! but cannot make sense of is an error, never a guess.

use crate::project_manifest::{PackageKind, ProjectManifest};

/// Why a `package.kira` declaration could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeclarationError {
    /// The file did not open with a `Package <name> {` header.
    #[error("not a package declaration: expected `Package <name> {{`")]
    MissingHeader,
    /// The `Package` header named no package.
    #[error("the `Package` declaration has no name")]
    MissingName,
    /// A `kind` was written that names neither an app nor a library.
    #[error("`{0}` is not a package kind (expected `.App` or `.Library`)")]
    UnknownKind(String),
    /// A value this reader understands was written in a shape it does not.
    #[error("the value of `{key}` is malformed")]
    MalformedValue {
        /// The key whose value could not be read.
        key: String,
    },
}

/// Reads a `package.kira` declaration into a [`ProjectManifest`].
///
/// Takes the text rather than a path: this crate stays filesystem-free at its
/// core, and the caller that found the file is the one that read it.
pub fn load(text: &str) -> Result<ProjectManifest, DeclarationError> {
    let (name, body) = split_header(text)?;
    let mut manifest = ProjectManifest::new(name, "0.1.0");
    for (key, value) in entries(body) {
        match key {
            "version" => manifest.version = string_value(key, value)?,
            "kira" => manifest.kira_version = string_value(key, value)?,
            "moduleRoot" => manifest.module_root = Some(string_value(key, value)?),
            "kind" => manifest.kind = kind_value(value)?,
            // Every other key belongs to a part of the model this reader does
            // not fill in yet. Ignored, not rejected: see the module docs.
            _ => {}
        }
    }
    Ok(manifest)
}

/// Splits `Package <name> { <body> }` into the name and the body.
fn split_header(text: &str) -> Result<(&str, &str), DeclarationError> {
    let start = text
        .find("Package")
        .ok_or(DeclarationError::MissingHeader)?;
    let after = &text[start + "Package".len()..];
    let open = after.find('{').ok_or(DeclarationError::MissingHeader)?;
    let name = after[..open].trim();
    if name.is_empty() {
        return Err(DeclarationError::MissingName);
    }
    let close = after.rfind('}').ok_or(DeclarationError::MissingHeader)?;
    if close < open {
        return Err(DeclarationError::MissingHeader);
    }
    Ok((name, &after[open + 1..close]))
}

/// Yields every top-level `let <key> = <value>` in a declaration body.
///
/// Nested braces are skipped wholesale, so a `Defaults { ... }` value never
/// leaks its own `key: value` pairs into the top level.
fn entries(body: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = rest.find("let ") {
        let line = &rest[at + "let ".len()..];
        let Some(eq) = line.find('=') else {
            break;
        };
        let key = line[..eq].trim();
        let value_start = &line[eq + 1..];
        let value = value_of(value_start);
        if !key.is_empty() {
            out.push((key, value));
        }
        rest = &value_start[value.len()..];
    }
    out
}

/// The text of one value: to the end of a balanced brace group, or to the end
/// of the line.
fn value_of(text: &str) -> &str {
    let trimmed = text.trim_start();
    let lead = text.len() - trimmed.len();
    if let Some(brace) = brace_group_end(trimmed) {
        return &text[..lead + brace];
    }
    let end = trimmed.find('\n').unwrap_or(trimmed.len());
    &text[..lead + end]
}

/// The byte offset just past a leading balanced `Ident { ... }` group, if the
/// value is one.
fn brace_group_end(text: &str) -> Option<usize> {
    let open = text.find('{')?;
    // A brace on a later line belongs to a later value, not this one.
    if text[..open].contains('\n') {
        return None;
    }
    let mut depth = 0usize;
    for (offset, ch) in text.char_indices().skip(open) {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads a `"quoted"` value.
fn string_value(key: &str, value: &str) -> Result<String, DeclarationError> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or_else(|| DeclarationError::MalformedValue {
            key: key.to_owned(),
        })
}

/// Reads a `kind` value: `.App`, `.Library`, or a qualified `PackageKind.App`.
///
/// Both spellings appear in the pinned corpus, so both are read; neither is
/// invented here.
fn kind_value(value: &str) -> Result<PackageKind, DeclarationError> {
    let trimmed = value.trim();
    let case = trimmed.rsplit('.').next().unwrap_or(trimmed).trim();
    match case {
        "App" => Ok(PackageKind::App),
        "Library" => Ok(PackageKind::Library),
        _ => Err(DeclarationError::UnknownKind(trimmed.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIBRARY: &str = "Package DemoLibrary {\n\
        \x20   let version = \"0.2.0\"\n\
        \x20   let kind = .Library\n\
        \x20   let moduleRoot = \"DemoLibrary\"\n\
        \x20   let defaults = Defaults { executionMode: .Vm, buildTarget: .Host }\n\
        }\n";

    const APP: &str = "Package DemoApp {\n\
        \x20   let version = \"0.1.0\"\n\
        \x20   let kind = .App\n\
        \x20   let defaults = Defaults { executionMode: .Vm, buildTarget: .Host }\n\
        }\n";

    #[test]
    fn reads_the_pinned_library_template() {
        let manifest = load(LIBRARY).unwrap();
        assert_eq!(manifest.name, "DemoLibrary");
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.kind, PackageKind::Library);
        assert_eq!(manifest.module_root.as_deref(), Some("DemoLibrary"));
    }

    #[test]
    fn reads_the_pinned_app_template() {
        let manifest = load(APP).unwrap();
        assert_eq!(manifest.name, "DemoApp");
        assert_eq!(manifest.kind, PackageKind::App);
        assert_eq!(manifest.module_root, None);
    }

    #[test]
    fn reads_the_qualified_kind_spelling() {
        // Both `.App` and `PackageKind.App` appear in the pinned corpus.
        let text = "Package p {\n let kind = PackageKind.Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
    }

    #[test]
    fn a_manifest_with_no_kind_is_an_app() {
        // `ProjectManifest::new`'s default, unchanged: a root package that does
        // not say is the runnable kind.
        let text = "Package p {\n let version = \"1.0.0\"\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::App);
    }

    #[test]
    fn a_nested_defaults_block_does_not_leak_keys() {
        // The `Defaults { ... }` value contains `buildTarget:`; reading it as a
        // top-level entry would silently pick up a field that is not one.
        let text = "Package p {\n \
            let defaults = Defaults { executionMode: .Vm, buildTarget: .Host }\n \
            let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
    }

    #[test]
    fn an_unknown_kind_is_refused_by_name() {
        let text = "Package p {\n let kind = .Plugin\n}";
        assert_eq!(
            load(text).unwrap_err(),
            DeclarationError::UnknownKind(".Plugin".to_owned())
        );
    }

    #[test]
    fn an_unquoted_version_is_refused() {
        let text = "Package p {\n let version = 0.1.0\n}";
        assert_eq!(
            load(text).unwrap_err(),
            DeclarationError::MalformedValue {
                key: "version".to_owned()
            }
        );
    }

    #[test]
    fn a_file_that_is_not_a_declaration_is_refused() {
        assert_eq!(
            load("@Main function main() { return }").unwrap_err(),
            DeclarationError::MissingHeader
        );
    }

    #[test]
    fn a_nameless_package_is_refused() {
        assert_eq!(
            load("Package { let kind = .App }").unwrap_err(),
            DeclarationError::MissingName
        );
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_rejected() {
        let text = "Package p {\n let somethingNew = \"x\"\n let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
    }
}
