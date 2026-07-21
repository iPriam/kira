//! Parses `NativeLibs/<name>.toml` text into the native-library model.

use kira_native_lib_definition::{
    NativeLibraryError, NativeLibraryManifest, NativeTargetRow, TargetTriple, TripleError,
};

use crate::native_lib_manifest::RawNativeLibManifest;

/// Parses a `NativeLibs/<name>.toml` document into the validated model.
///
/// Deserializes the raw TOML, parses each row's triple, then hands the rows to
/// [`NativeLibraryManifest::new`], which rejects a pathless row or a duplicate
/// target. Every failure is a typed [`NativeLibParseError`].
pub fn parse_native_lib_manifest(text: &str) -> Result<NativeLibraryManifest, NativeLibParseError> {
    let raw: RawNativeLibManifest = toml::from_str(text)?;
    let mut rows = Vec::with_capacity(raw.target.len());
    for target in raw.target {
        let triple = TargetTriple::parse(&target.triple)?;
        rows.push(NativeTargetRow::new(triple, target.static_lib));
    }
    Ok(NativeLibraryManifest::new(raw.name, rows)?)
}

/// Why a `NativeLibs/*.toml` document could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum NativeLibParseError {
    /// The document was not well-formed TOML or did not match the schema.
    #[error("malformed native-library manifest: {0}")]
    Toml(#[from] toml::de::Error),
    /// A row named a triple that is not `arch-os-abi`.
    #[error(transparent)]
    Triple(#[from] TripleError),
    /// The rows were well-formed but did not validate as a manifest.
    #[error(transparent)]
    Model(#[from] NativeLibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID: &str = r#"
name = "ffimath"

[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffimath-macos.a"

[[target]]
triple = "wasm32-emscripten-unknown"
staticLib = "lib/libffimath-wasm.a"
"#;

    #[test]
    fn parses_a_two_target_manifest() {
        let manifest = parse_native_lib_manifest(VALID).expect("a valid manifest");
        assert_eq!(manifest.name(), "ffimath");
        assert_eq!(manifest.targets().len(), 2);
        assert_eq!(
            manifest.targets()[0].triple(),
            &TargetTriple::parse("aarch64-macos-none").unwrap()
        );
        assert_eq!(manifest.targets()[1].static_lib(), "lib/libffimath-wasm.a");
    }

    #[test]
    fn a_malformed_triple_is_a_typed_error() {
        let text = r#"
name = "ffimath"
[[target]]
triple = "aarch64_macos"
staticLib = "lib/host.a"
"#;
        assert!(matches!(
            parse_native_lib_manifest(text),
            Err(NativeLibParseError::Triple(TripleError::Malformed { .. }))
        ));
    }

    #[test]
    fn a_pathless_row_is_a_typed_error() {
        let text = r#"
name = "ffimath"
[[target]]
triple = "aarch64-macos-none"
staticLib = ""
"#;
        assert!(matches!(
            parse_native_lib_manifest(text),
            Err(NativeLibParseError::Model(
                NativeLibraryError::PathlessRow { .. }
            ))
        ));
    }

    #[test]
    fn a_duplicate_target_is_a_typed_error() {
        let text = r#"
name = "ffimath"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/a.a"
[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/b.a"
"#;
        assert!(matches!(
            parse_native_lib_manifest(text),
            Err(NativeLibParseError::Model(
                NativeLibraryError::DuplicateTarget { .. }
            ))
        ));
    }

    #[test]
    fn malformed_toml_is_a_typed_error() {
        assert!(matches!(
            parse_native_lib_manifest("name = "),
            Err(NativeLibParseError::Toml(_))
        ));
    }
}
