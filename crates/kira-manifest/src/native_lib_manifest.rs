//! Raw serde shapes for `NativeLibs/*.toml`, converted to the model in
//! [`crate::native_lib_parser`].
//!
//! These mirror the TOML text exactly (including the `staticLib` spelling) and
//! carry no validation; converting them into the validated model
//! [`kira_native_lib_definition::NativeLibraryManifest`] is the parser's job.

use serde::Deserialize;

/// The top-level `NativeLibs/<name>.toml` document.
///
/// ```toml
/// name = "ffimath"
/// [[target]]
/// triple = "aarch64-macos-none"
/// staticLib = "lib/libffimath-macos.a"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RawNativeLibManifest {
    /// The library name.
    pub name: String,
    /// The per-target archive rows. Absent means an empty list.
    #[serde(default)]
    pub target: Vec<RawNativeTarget>,
}

/// One `[[target]]` row, spelled as it appears in the TOML.
#[derive(Debug, Clone, Deserialize)]
pub struct RawNativeTarget {
    /// The `arch-os-abi` target triple.
    pub triple: String,
    /// The archive path relative to this manifest, spelled `staticLib`.
    #[serde(rename = "staticLib")]
    pub static_lib: String,
}
