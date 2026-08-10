//! Raw serde shapes for `NativeLibs/*.toml`, converted to the model in
//! [`crate::native_lib_parser`].
//!
//! Two spellings exist in the pinned corpus and both are read here. The flat
//! one puts `name` at the top level and each target in a `[[target]]` row; the
//! sectioned one groups the library under `[library]` and keys targets by
//! triple (`[target.aarch64-macos-none]`). They are separate types because the
//! `target` key is an array in one and a table in the other — one serde struct
//! cannot describe both.
//!
//! These mirror the TOML text exactly and carry no validation; converting them
//! into the validated [`kira_native_lib_definition::NativeLibrarySpec`] is the
//! parser's job.

use std::collections::BTreeMap;

use serde::Deserialize;

/// The flat spelling of a `NativeLibs/<name>.toml` document.
///
/// ```toml
/// name = "ffimath"
/// [[target]]
/// triple = "aarch64-macos-none"
/// staticLib = "lib/libffimath-macos.a"
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RawFlatManifest {
    /// The library name.
    pub name: String,
    /// The per-target archive rows. Absent means an empty list.
    #[serde(default)]
    pub target: Vec<RawFlatTarget>,
}

/// One `[[target]]` row of the flat spelling.
#[derive(Debug, Clone, Deserialize)]
pub struct RawFlatTarget {
    /// The `arch-os-abi` target triple.
    pub triple: String,
    /// The archive path relative to this manifest, spelled `staticLib`.
    #[serde(rename = "staticLib")]
    pub static_lib: String,
}

/// The sectioned spelling of a `NativeLibs/<name>.toml` document.
///
/// ```toml
/// [library]
/// name = "sokol"
/// link_mode = "static"
///
/// [target.aarch64-macos-none]
/// static_lib = "../generated/native/aarch64-macos/libsokol.a"
/// frameworks = ["AppKit", "QuartzCore"]
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct RawSectionedManifest {
    /// The `[library]` section naming the library and how it links.
    pub library: RawLibrarySection,
    /// The `[headers]` section.
    pub headers: Option<RawHeadersSection>,
    /// The `[autobinding]` section.
    pub autobinding: Option<RawAutobindingSection>,
    /// The `[bindings]` section refining how much autobinding exposes.
    pub bindings: Option<RawBindingsSection>,
    /// The `[build]` section naming the library's own C sources.
    pub build: Option<RawBuildSection>,
    /// The `[target.<triple>]` sections, keyed by triple. A `BTreeMap` so the
    /// rows come out in a stable order regardless of how the file was written.
    #[serde(default)]
    pub target: BTreeMap<String, RawSectionedTarget>,
}

/// The `[library]` section.
#[derive(Debug, Clone, Deserialize)]
pub struct RawLibrarySection {
    /// The library name.
    pub name: String,
    /// `"static"`, `"dynamic"`, or `"runtime"`; absent means static.
    pub link_mode: Option<String>,
    /// `"required"` or `"optional"`; absent means required.
    pub availability: Option<String>,
}

/// The `[headers]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawHeadersSection {
    /// The single header that includes the rest.
    pub entrypoint: Option<String>,
    /// Include directories relative to this manifest.
    #[serde(default)]
    pub include_dirs: Vec<String>,
    /// Preprocessor defines applied on every target.
    #[serde(default)]
    pub defines: Vec<String>,
}

/// The `[autobinding]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawAutobindingSection {
    /// The Kira module generated bindings land in.
    pub module: Option<String>,
    /// Where generated bindings are written.
    pub output: Option<String>,
    /// The headers to generate from.
    #[serde(default)]
    pub headers: Vec<String>,
    /// Individually named functions to bind.
    #[serde(default)]
    pub functions: Vec<String>,
    /// Individually named structs to bind.
    #[serde(default)]
    pub structs: Vec<String>,
}

/// The `[bindings]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawBindingsSection {
    /// `"all_public"` or `"selected"`.
    pub mode: Option<String>,
    /// The generator ruleset name.
    pub profile: Option<String>,
}

/// The `[build]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawBuildSection {
    /// The C sources compiled into the library.
    #[serde(default)]
    pub sources: Vec<String>,
    /// Include directories used when compiling those sources.
    #[serde(default)]
    pub include_dirs: Vec<String>,
}

/// One `[target.<triple>]` section.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct RawSectionedTarget {
    /// The static archive path relative to this manifest.
    pub static_lib: Option<String>,
    /// The shared library path relative to this manifest. An empty string means
    /// the loader finds the library by its install name.
    pub dynamic_lib: Option<String>,
    /// Preprocessor defines for this target.
    #[serde(default)]
    pub defines: Vec<String>,
    /// Apple frameworks to link on this target.
    #[serde(default)]
    pub frameworks: Vec<String>,
    /// System libraries to link on this target.
    #[serde(default)]
    pub system_libs: Vec<String>,
    /// C compiler flags for this target.
    #[serde(default)]
    pub compiler_flags: Vec<String>,
    /// Linker flags for this target.
    #[serde(default)]
    pub linker_flags: Vec<String>,
    /// Files the finished program must find beside itself at run time — a
    /// shared library the loader opens, or a directory holding several.
    #[serde(default)]
    pub runtime_files: Vec<String>,
}
