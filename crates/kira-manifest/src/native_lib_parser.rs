//! Parses `NativeLibs/<name>.toml` text into the native-library model.

use kira_native_lib_definition::{
    AutobindMode, AutobindProfile, AutobindSpec, Availability, LinkMode, NativeArtifact,
    NativeHeaders, NativeLibraryError, NativeLibrarySpec, NativeLinkAttributes, NativeTargetSpec,
    TargetTriple, TripleError,
};

use crate::native_lib_manifest::{RawFlatManifest, RawSectionedManifest, RawSectionedTarget};

/// Parses a `NativeLibs/<name>.toml` document into the validated model.
///
/// Both corpus spellings are accepted — the flat `name` + `[[target]]` form and
/// the sectioned `[library]` + `[target.<triple>]` form — and both decode into
/// one [`NativeLibrarySpec`], so where a library was written never changes what
/// it means. The presence of a `[library]` table selects the sectioned form.
/// Every failure is a typed [`NativeLibParseError`].
pub fn parse_native_lib_manifest(text: &str) -> Result<NativeLibrarySpec, NativeLibParseError> {
    let document: toml::Table = toml::from_str(text)?;
    let sectioned_form = document.contains_key("library");
    let document = toml::Value::Table(document);
    if sectioned_form {
        sectioned(document.try_into()?)
    } else {
        flat(document.try_into()?)
    }
}

/// Converts the flat spelling into the model.
fn flat(raw: RawFlatManifest) -> Result<NativeLibrarySpec, NativeLibParseError> {
    let mut rows = Vec::with_capacity(raw.target.len());
    for target in raw.target {
        let triple = TargetTriple::parse(&target.triple)?;
        rows.push(NativeTargetSpec::static_archive(triple, target.static_lib));
    }
    Ok(NativeLibrarySpec::new(raw.name, LinkMode::Static, rows)?)
}

/// Converts the sectioned spelling into the model.
fn sectioned(raw: RawSectionedManifest) -> Result<NativeLibrarySpec, NativeLibParseError> {
    let link_mode = match raw.library.link_mode.as_deref() {
        None => LinkMode::Static,
        Some(text) => {
            LinkMode::parse(text).ok_or_else(|| NativeLibParseError::UnknownLinkMode {
                value: text.to_owned(),
            })?
        }
    };

    let mut rows = Vec::with_capacity(raw.target.len());
    for (triple, target) in raw.target {
        rows.push(target_row(TargetTriple::parse(&triple)?, target));
    }

    let availability = match raw.library.availability.as_deref() {
        None => Availability::Required,
        Some(text) => {
            Availability::parse(text).ok_or_else(|| NativeLibParseError::UnknownAvailability {
                value: text.to_owned(),
            })?
        }
    };

    let mut spec =
        NativeLibrarySpec::new(raw.library.name, link_mode, rows)?.with_availability(availability);
    if let Some(headers) = raw.headers {
        spec = spec.with_headers(NativeHeaders {
            entrypoint: headers.entrypoint,
            include_dirs: headers.include_dirs,
            defines: headers.defines,
        });
    }
    if let Some(build) = raw.build {
        spec = spec.with_sources(build.sources);
    }
    if let Some(autobinding) = raw.autobinding {
        let bindings = raw.bindings.unwrap_or_default();
        let mode = match bindings.mode.as_deref() {
            None => AutobindMode::default(),
            Some(text) => AutobindMode::parse(text).ok_or_else(|| {
                NativeLibParseError::UnknownAutobindMode {
                    value: text.to_owned(),
                }
            })?,
        };
        spec = spec.with_autobind(AutobindSpec {
            module: autobinding.module,
            headers: autobinding.headers,
            functions: autobinding.functions,
            structs: autobinding.structs,
            mode,
            profile: bindings.profile.map(AutobindProfile::new),
            output: autobinding.output,
        });
    }
    Ok(spec)
}

/// Builds one target row from a `[target.<triple>]` section.
fn target_row(triple: TargetTriple, target: RawSectionedTarget) -> NativeTargetSpec {
    let artifact =
        NativeArtifact::from_paths(target.static_lib.as_deref(), target.dynamic_lib.as_deref());
    NativeTargetSpec::new(triple, artifact)
        .with_defines(target.defines)
        .with_attributes(NativeLinkAttributes {
            frameworks: target.frameworks,
            system_libs: target.system_libs,
            compiler_flags: target.compiler_flags,
            linker_flags: target.linker_flags,
            runtime_files: target.runtime_files,
        })
}

/// Why a `NativeLibs/*.toml` document could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum NativeLibParseError {
    /// The document was not well-formed TOML or did not match either schema.
    #[error("malformed native-library manifest: {0}")]
    Toml(#[from] toml::de::Error),
    /// A row named a triple that is not `arch-os-abi`.
    #[error(transparent)]
    Triple(#[from] TripleError),
    /// `link_mode` named none of the three modes.
    #[error("`{value}` is not a link mode (expected `static`, `dynamic`, or `runtime`)")]
    UnknownLinkMode {
        /// The unreadable link mode.
        value: String,
    },
    /// `availability` named neither `required` nor `optional`.
    #[error("`{value}` is not an availability (expected `required` or `optional`)")]
    UnknownAvailability {
        /// The unreadable availability.
        value: String,
    },
    /// The binding `mode` named neither `all_public` nor `selected`.
    #[error("`{value}` is not a binding mode (expected `all_public` or `selected`)")]
    UnknownAutobindMode {
        /// The unreadable binding mode.
        value: String,
    },
    /// The rows were well-formed but did not validate as a declaration.
    #[error(transparent)]
    Model(#[from] NativeLibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLAT: &str = r#"
name = "ffimath"

[[target]]
triple = "aarch64-macos-none"
staticLib = "lib/libffimath-macos.a"

[[target]]
triple = "wasm32-emscripten-unknown"
staticLib = "lib/libffimath-wasm.a"
"#;

    // The shape the pinned corpus actually ships (`kira-swift`'s sokol.toml,
    // `kira-graphics`'s DirectX12.toml): sectioned, triple-keyed targets.
    const SECTIONED: &str = r#"
[library]
name = "sokol"
link_mode = "static"
abi = "c"

[headers]
entrypoint = "../../third_party/sokol/sokol_bindings.h"
include_dirs = ["../../third_party/sokol"]
defines = ["SOKOL_NO_ENTRY"]

[autobinding]
module = "sokol"
output = "../sokol.kira"
headers = ["../../third_party/sokol/sokol_app.h"]

[bindings]
mode = "all_public"

[build]
sources = ["../../third_party/sokol/sokol_impl.m"]
include_dirs = ["../../third_party/sokol"]

[target.aarch64-macos-none]
static_lib = "../generated/native/aarch64-macos/libsokol.a"
frameworks = ["AppKit", "QuartzCore", "OpenGL"]

[target.x86_64-linux-gnu]
static_lib = "../generated/native/x86_64-linux-gnu/libsokol.a"
system_libs = ["X11", "GL"]
"#;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    #[test]
    fn parses_a_two_target_flat_manifest() {
        let spec = parse_native_lib_manifest(FLAT).expect("a valid manifest");
        assert_eq!(spec.name(), "ffimath");
        assert_eq!(spec.targets().len(), 2);
        assert_eq!(spec.targets()[0].triple(), &triple("aarch64-macos-none"));
        assert_eq!(
            spec.targets()[1].artifact(),
            &NativeArtifact::StaticArchive("lib/libffimath-wasm.a".to_owned())
        );
    }

    #[test]
    fn parses_the_sectioned_corpus_spelling() {
        let spec = parse_native_lib_manifest(SECTIONED).expect("a valid manifest");
        assert_eq!(spec.name(), "sokol");
        assert_eq!(spec.link_mode(), LinkMode::Static);
        assert_eq!(spec.sources(), ["../../third_party/sokol/sokol_impl.m"]);
        let headers = spec.headers().expect("a headers section");
        assert_eq!(headers.defines, ["SOKOL_NO_ENTRY"]);
        let autobind = spec.autobind().expect("an autobinding section");
        assert_eq!(autobind.mode, AutobindMode::AllPublic);
        assert_eq!(autobind.module.as_deref(), Some("sokol"));

        // Triple-keyed sections come out in a stable order, and each row keeps
        // the frameworks and system libraries the link line needs.
        assert_eq!(spec.targets().len(), 2);
        let macos = spec
            .targets()
            .iter()
            .find(|row| row.triple() == &triple("aarch64-macos-none"))
            .expect("the macOS row");
        assert_eq!(
            macos.attributes().frameworks,
            ["AppKit", "QuartzCore", "OpenGL"]
        );
        let linux = spec
            .targets()
            .iter()
            .find(|row| row.triple() == &triple("x86_64-linux-gnu"))
            .expect("the Linux row");
        assert_eq!(linux.attributes().system_libs, ["X11", "GL"]);
    }

    #[test]
    fn reads_the_pathless_dynamic_row_the_corpus_ships() {
        // `kira-graphics`'s DirectX12.toml, verbatim in shape: a dynamic
        // library found by its own name, declaring no path.
        let text = r#"
[library]
name = "directx12"
link_mode = "dynamic"
abi = "c"

[target.x86_64-windows-msvc]
dynamic_lib = ""
"#;
        let spec = parse_native_lib_manifest(text).expect("a valid manifest");
        assert_eq!(spec.link_mode(), LinkMode::Dynamic);
        assert_eq!(spec.targets()[0].artifact(), &NativeArtifact::None);
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
    fn an_unknown_link_mode_is_a_typed_error() {
        let text = r#"
[library]
name = "ffimath"
link_mode = "weak"

[target.aarch64-macos-none]
static_lib = "lib/host.a"
"#;
        assert!(matches!(
            parse_native_lib_manifest(text),
            Err(NativeLibParseError::UnknownLinkMode { .. })
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
