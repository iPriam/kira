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
//!     let dependencies = [Dependency { name: "Core", path: "../core" }]
//!     let defaults = Defaults { executionMode: .Vm, buildTarget: .Host }
//! }
//! ```
//!
//! Version, Kira version, module root, kind, dependencies, defaults, and the
//! inline `nativeLibraries` array (read by [`crate::declaration_native_libs`])
//! are decoded. Unknown keys are ignored rather than rejected, because this
//! crate's model covers a subset of the fields a manifest may carry and
//! rejecting the rest would make every new field a breaking change. A key this
//! reader *does* know but cannot make sense of is an error, never a guess.

use kira_native_lib_definition::{NativeLibraryError, TripleError};

use crate::declaration_native_libs::native_libraries_value;
use crate::dependency::{DependencySource, DependencySpec, GitSource, PathSource, RegistrySource};
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
    /// A `nativeTargets` row named a triple that is not `arch-os-abi`.
    #[error(transparent)]
    Triple(#[from] TripleError),
    /// A `nativeLibraries` entry was well-formed but did not validate.
    ///
    /// Boxed because it is by far the largest thing that can go wrong here (a
    /// library name, a triple, and a path), and every `kira` verb reads a
    /// manifest — an unboxed variant would widen the `Result` of every function
    /// that returns this, and of `kira-project`'s discovery errors above it.
    #[error("the `nativeLibraries` declaration is invalid: {0}")]
    InvalidNativeLibrary(#[source] Box<NativeLibraryError>),
}

impl From<NativeLibraryError> for DeclarationError {
    fn from(error: NativeLibraryError) -> Self {
        Self::InvalidNativeLibrary(Box::new(error))
    }
}

/// Reads a `package.kira` declaration into a [`ProjectManifest`].
///
/// Takes the text rather than a path: this crate stays filesystem-free at its
/// core, and the caller that found the file is the one that read it.
pub fn load(text: &str) -> Result<ProjectManifest, DeclarationError> {
    let text = strip_comments(text);
    let (name, body) = split_header(&text)?;
    let mut manifest = ProjectManifest::new(name, "0.1.0");
    for (key, value) in entries(body) {
        match key {
            "version" => manifest.version = string_value(key, value)?,
            "kira" => manifest.kira_version = string_value(key, value)?,
            "moduleRoot" => manifest.module_root = Some(string_value(key, value)?),
            "kind" => manifest.kind = kind_value(value)?,
            "dependencies" => manifest.dependencies = dependencies_value(value)?,
            "nativeLibraries" => manifest.native_libraries = native_libraries_value(value)?,
            "defaults" => {
                let (execution_mode, build_target) = defaults_value(value)?;
                if let Some(mode) = execution_mode {
                    manifest.execution_mode = mode;
                }
                if let Some(target) = build_target {
                    manifest.build_target = target;
                }
            }
            // Deferred and unknown keys are ignored, not rejected: see the
            // module docs.
            _ => {}
        }
    }
    Ok(manifest)
}

/// Blanks out `//` line comments, preserving the text's length.
///
/// A manifest is commented like the Kira source it resembles, and a comment may
/// sit anywhere — including between two entries of a `nativeLibraries` array,
/// where it would otherwise become part of the record name that follows it. The
/// reader is offset-based throughout, so each comment byte becomes a space
/// rather than disappearing: nothing else has to know this ran.
///
/// String literals are respected. `Dependency { url: "https://example.test/r.git" }`
/// is a real corpus line, and a stripper that did not track quoting would eat
/// the rest of it.
fn strip_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut quoted = false;
    let mut escaped = false;
    let mut commented = false;
    let mut chars = text.char_indices().peekable();
    while let Some((_, ch)) = chars.next() {
        if commented {
            if ch == '\n' {
                commented = false;
                out.push(ch);
            } else {
                // One space per byte keeps every later offset in place, whatever
                // the comment happened to contain.
                out.push_str(&" ".repeat(ch.len_utf8()));
            }
            continue;
        }
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            out.push(ch);
            continue;
        }
        match ch {
            '"' => {
                quoted = true;
                out.push(ch);
            }
            '/' if chars.peek().is_some_and(|(_, next)| *next == '/') => {
                commented = true;
                out.push(' ');
            }
            _ => out.push(ch),
        }
    }
    out
}

/// True when `text[start..end]` is a whole word rather than part of a longer
/// identifier, so `MyPackage` and `outlet` are not mistaken for `Package` and
/// `let`.
fn is_whole_word(text: &str, start: usize, end: usize) -> bool {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let before_ok = text[..start].chars().next_back().is_none_or(|c| !ident(c));
    let after_ok = text[end..].chars().next().is_none_or(|c| !ident(c));
    before_ok && after_ok
}

/// Splits `Package <name> { <body> }` into the name and the body.
///
/// The header is the first whole-word `Package` that is followed by a single
/// identifier and a `{`. Scanning past a candidate whose name is not one
/// identifier is what keeps a leading `// Package manifest` comment from being
/// taken as the header and its multi-line remainder from becoming the name.
fn split_header(text: &str) -> Result<(&str, &str), DeclarationError> {
    let mut searched = 0usize;
    // A `Package ... {` was seen but never with a usable name: that is a named
    // failure (`MissingName`), not "this is not a declaration".
    let mut saw_open_header = false;
    while let Some(rel) = text[searched..].find("Package") {
        let start = searched + rel;
        let after_keyword = start + "Package".len();
        searched = after_keyword;
        if !is_whole_word(text, start, after_keyword) {
            continue;
        }
        let after = &text[after_keyword..];
        let Some(open) = after.find('{') else {
            continue;
        };
        saw_open_header = true;
        let name = after[..open].trim();
        // One identifier, nothing else: a name carrying whitespace means this
        // `Package` was not the header.
        if name.is_empty() || name.chars().any(char::is_whitespace) {
            continue;
        }
        let close = after.rfind('}').ok_or(DeclarationError::MissingHeader)?;
        if close < open {
            return Err(DeclarationError::MissingHeader);
        }
        return Ok((name, &after[open + 1..close]));
    }
    if saw_open_header {
        return Err(DeclarationError::MissingName);
    }
    Err(DeclarationError::MissingHeader)
}

/// Yields every top-level `let <key> = <value>` in a declaration body.
///
/// Nested braces are skipped wholesale, so a `Defaults { ... }` value never
/// leaks its own `key: value` pairs into the top level.
fn entries(body: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut rest = body;
    while let Some(at) = find_let(rest) {
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

/// The byte offset of the next whole-word `let ` binding, skipping an
/// identifier that merely ends in `let` (`outlet = ...`).
fn find_let(text: &str) -> Option<usize> {
    let mut searched = 0usize;
    while let Some(rel) = text[searched..].find("let ") {
        let at = searched + rel;
        searched = at + "let".len();
        if is_whole_word(text, at, at + "let".len()) {
            return Some(at);
        }
    }
    None
}

/// The text of one value: to the end of a balanced brace or bracket group, or
/// to the end of the line.
fn value_of(text: &str) -> &str {
    let trimmed = text.trim_start();
    let lead = text.len() - trimmed.len();
    let grouped_end = if trimmed.starts_with('[') {
        group_end(trimmed, 0)
    } else {
        brace_group_end(trimmed)
    };
    if let Some(end) = grouped_end {
        return &text[..lead + end];
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
    group_end(text, open)
}

/// Finds the end of a balanced brace or bracket group, including nested groups
/// and quoted strings that may contain delimiter characters.
fn group_end(text: &str, open: usize) -> Option<usize> {
    let mut closers = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for (offset, ch) in text[open..].char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' => closers.push('}'),
            '[' => closers.push(']'),
            '}' | ']' => {
                if closers.pop() != Some(ch) {
                    return None;
                }
                if closers.is_empty() {
                    return Some(open + offset + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Reads a `"quoted"` value.
pub(crate) fn string_value(key: &str, value: &str) -> Result<String, DeclarationError> {
    let trimmed = value.trim();
    trimmed
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
        .map(str::to_owned)
        .ok_or_else(|| DeclarationError::MalformedValue {
            key: key.to_owned(),
        })
}

/// Reads the dependency array from a declaration.
fn dependencies_value(value: &str) -> Result<Vec<DependencySpec>, DeclarationError> {
    let mut dependencies = Vec::new();
    for item in array_items("dependencies", value)? {
        dependencies.push(dependency_value(item)?);
    }
    Ok(dependencies)
}

/// Reads one `Dependency { ... }` entry while tolerating fields not modeled yet.
fn dependency_value(value: &str) -> Result<DependencySpec, DeclarationError> {
    let mut name = None;
    let mut path = None;
    let mut version = None;
    let mut url = None;
    let mut rev = None;
    let mut tag = None;
    for (field, value) in record_fields("dependencies", "Dependency", value)? {
        let slot = match field {
            "name" => Some(&mut name),
            "path" => Some(&mut path),
            "version" => Some(&mut version),
            "url" => Some(&mut url),
            "rev" => Some(&mut rev),
            "tag" => Some(&mut tag),
            _ => None,
        };
        if let Some(slot) = slot {
            if slot.is_some() {
                return Err(malformed("dependencies"));
            }
            *slot = Some(non_empty_string("dependencies", value)?);
        }
    }
    let name = name.ok_or_else(|| malformed("dependencies"))?;
    let source = match (path, version, url) {
        (Some(path), None, None) if rev.is_none() && tag.is_none() => {
            DependencySource::Path(PathSource { path })
        }
        (None, Some(version), None) if rev.is_none() && tag.is_none() => {
            DependencySource::Registry(RegistrySource { version })
        }
        (None, None, Some(url)) => DependencySource::Git(GitSource { url, rev, tag }),
        _ => return Err(malformed("dependencies")),
    };
    Ok(DependencySpec { name, source })
}

/// Reads execution and target defaults while leaving absent fields unchanged.
fn defaults_value(value: &str) -> Result<(Option<String>, Option<String>), DeclarationError> {
    let mut execution_mode = None;
    let mut build_target = None;
    for (field, value) in record_fields("defaults", "Defaults", value)? {
        match field {
            "executionMode" => {
                if execution_mode.is_some() {
                    return Err(malformed("defaults"));
                }
                execution_mode = Some(match qualified_case(value) {
                    "Vm" => "vm".to_owned(),
                    "Llvm" => "llvm".to_owned(),
                    "Hybrid" => "hybrid".to_owned(),
                    _ => return Err(malformed("defaults")),
                });
            }
            "buildTarget" => {
                if build_target.is_some() {
                    return Err(malformed("defaults"));
                }
                build_target = Some(match qualified_case(value) {
                    "Host" => "host".to_owned(),
                    "Wasm32" => "wasm32".to_owned(),
                    "Wasm64" => "wasm64".to_owned(),
                    _ => return Err(malformed("defaults")),
                });
            }
            _ => {}
        }
    }
    Ok((execution_mode, build_target))
}

/// Returns the comma-separated items inside a balanced array.
pub(crate) fn array_items<'a>(key: &str, value: &'a str) -> Result<Vec<&'a str>, DeclarationError> {
    let trimmed = value.trim();
    if !trimmed.starts_with('[') || group_end(trimmed, 0) != Some(trimmed.len()) {
        return Err(malformed(key));
    }
    comma_separated(key, &trimmed[1..trimmed.len() - 1])
}

/// Returns the fields of a named brace record.
pub(crate) fn record_fields<'a>(
    key: &str,
    record: &str,
    value: &'a str,
) -> Result<Vec<(&'a str, &'a str)>, DeclarationError> {
    let trimmed = value.trim();
    let open = trimmed.find('{').ok_or_else(|| malformed(key))?;
    if trimmed[..open].trim() != record || brace_group_end(trimmed) != Some(trimmed.len()) {
        return Err(malformed(key));
    }
    let mut fields = Vec::new();
    for item in comma_separated(key, &trimmed[open + 1..trimmed.len() - 1])? {
        let colon = item.find(':').ok_or_else(|| malformed(key))?;
        let field = item[..colon].trim();
        let value = item[colon + 1..].trim();
        if field.is_empty()
            || !field.chars().all(|ch| ch.is_alphanumeric() || ch == '_')
            || value.is_empty()
        {
            return Err(malformed(key));
        }
        fields.push((field, value));
    }
    Ok(fields)
}

/// Splits top-level comma-separated values, preserving nested records and arrays.
fn comma_separated<'a>(key: &str, mut text: &'a str) -> Result<Vec<&'a str>, DeclarationError> {
    let mut values = Vec::new();
    while !text.trim().is_empty() {
        text = text.trim_start();
        let comma = top_level_comma(text).map_err(|()| malformed(key))?;
        let end = comma.unwrap_or(text.len());
        let value = text[..end].trim();
        if value.is_empty() {
            return Err(malformed(key));
        }
        values.push(value);
        let Some(comma) = comma else {
            break;
        };
        text = &text[comma + 1..];
    }
    Ok(values)
}

/// Finds a comma outside quoted strings and nested brace or bracket groups.
fn top_level_comma(text: &str) -> Result<Option<usize>, ()> {
    let mut closers = Vec::new();
    let mut quoted = false;
    let mut escaped = false;
    for (offset, ch) in text.char_indices() {
        if quoted {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                quoted = false;
            }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' => closers.push('}'),
            '[' => closers.push(']'),
            '}' | ']' if closers.pop() != Some(ch) => return Err(()),
            ',' if closers.is_empty() => return Ok(Some(offset)),
            _ => {}
        }
    }
    if quoted || !closers.is_empty() {
        Err(())
    } else {
        Ok(None)
    }
}

/// Reads a non-empty quoted string.
pub(crate) fn non_empty_string(key: &str, value: &str) -> Result<String, DeclarationError> {
    let value = string_value(key, value)?;
    if value.is_empty() {
        Err(malformed(key))
    } else {
        Ok(value)
    }
}

/// Constructs the uniform error for a known key with an unreadable value.
pub(crate) fn malformed(key: &str) -> DeclarationError {
    DeclarationError::MalformedValue {
        key: key.to_owned(),
    }
}

/// Returns the final case name from `.Case` or `Qualified.Case`.
pub(crate) fn qualified_case(value: &str) -> &str {
    let trimmed = value.trim();
    trimmed.rsplit('.').next().unwrap_or(trimmed).trim()
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
    fn a_non_ascii_character_before_a_brace_does_not_panic() {
        // Regression: `brace_group_end` used the byte offset of `{` as a count
        // of characters to skip, so a multi-byte character before the brace
        // started iteration past it and the first `}` underflowed the depth.
        // Every `kira` verb reads a manifest, so this decoder must refuse
        // malformed input rather than abort the process.
        let text = "Package p {\n let note = é {}\n let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);

        let quoted = "Package p {\n let s = \"café\" {}\n let kind = .Library\n}";
        assert_eq!(load(quoted).unwrap().kind, PackageKind::Library);
    }

    #[test]
    fn a_non_ascii_value_never_panics_on_any_prefix() {
        // Every byte-boundary-respecting prefix of a manifest full of
        // multi-byte text is either read or refused by name; none may panic.
        let text = "Package pé {\n let version = \"1.0.0\"\n \
            let defaults = Defaults { mode: .Vm, note: \"héllo → wörld\" }\n \
            let kind = .Library\n}";
        for end in 0..=text.len() {
            if text.is_char_boundary(end) {
                let _ = load(&text[..end]);
            }
        }
    }

    #[test]
    fn a_leading_comment_mentioning_package_is_not_the_header() {
        // Regression: anchoring on the first `Package` in the file made the
        // name the whole `manifest\nPackage demo` run, which parsed as a
        // package silently named garbage.
        let text = "// Package manifest for the demo\nPackage demo {\n let kind = .Library\n}";
        let manifest = load(text).unwrap();
        assert_eq!(manifest.name, "demo");
        assert_eq!(manifest.kind, PackageKind::Library);
    }

    #[test]
    fn a_header_whose_name_is_not_one_identifier_is_refused() {
        assert_eq!(
            load("// Package notes here\n").unwrap_err(),
            DeclarationError::MissingHeader
        );
        assert_eq!(
            load("Package two names {\n let kind = .App\n}").unwrap_err(),
            DeclarationError::MissingName
        );
    }

    #[test]
    fn an_identifier_ending_in_let_is_not_a_binding() {
        // Regression: `find("let ")` matched inside `outlet `, which yielded an
        // empty key and swallowed the real value that followed it.
        let text = "Package p {\n outlet = \"x\"\n let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
        // And a genuine binding named `outlet` still reads as one.
        let bound = "Package p {\n let outlet = \"x\"\n let kind = .Library\n}";
        assert_eq!(load(bound).unwrap().kind, PackageKind::Library);
    }

    #[test]
    fn a_comment_between_array_entries_is_not_part_of_the_next_entry() {
        // Regression: `ui-foundation`'s manifest comments its third
        // `NativeLibrary`, and the comment ran into the record name that
        // followed it — so the whole package failed to load and every module
        // importing it went undefined.
        let text = "Package p {\n\
            \x20   let dependencies = [\n\
            \x20       Dependency { name: \"A\", path: \"../a\" },\n\
            \x20       // why B is here\n\
            \x20       Dependency { name: \"B\", path: \"../b\" }\n\
            \x20   ]\n\
            }";
        let manifest = load(text).expect("a commented array reads");
        let names: Vec<&str> = manifest
            .dependencies
            .iter()
            .map(|dependency| dependency.name.as_str())
            .collect();
        assert_eq!(names, ["A", "B"]);
    }

    #[test]
    fn a_double_slash_inside_a_string_is_not_a_comment() {
        let text = "Package p {\n \
            let dependencies = [Dependency { name: \"G\", url: \"https://x.test/r.git\" }]\n \
            let kind = .Library\n}";
        let manifest = load(text).expect("a url survives comment stripping");
        assert_eq!(manifest.kind, PackageKind::Library);
        assert_eq!(
            manifest.dependencies[0].source,
            DependencySource::Git(GitSource {
                url: "https://x.test/r.git".to_owned(),
                rev: None,
                tag: None,
            })
        );
    }

    #[test]
    fn a_comment_holding_multi_byte_text_does_not_shift_what_follows() {
        let text = "Package p {\n // héllo → wörld\n let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
    }

    #[test]
    fn an_unknown_key_is_ignored_rather_than_rejected() {
        let text = "Package p {\n let somethingNew = \"x\"\n let kind = .Library\n}";
        assert_eq!(load(text).unwrap().kind, PackageKind::Library);
    }
}
