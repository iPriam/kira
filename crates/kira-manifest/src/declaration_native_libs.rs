//! Reads the inline `nativeLibraries` declaration out of a `package.kira`.
//!
//! A package may declare its C libraries in either of two places, and the
//! corpus uses both: a `NativeLibs/<name>.toml` beside the package, or inline
//! in the manifest itself —
//!
//! ```text
//! let nativeLibraries = [
//!     NativeLibrary {
//!         name: "sokol",
//!         linkMode: LinkMode.Static,
//!         headers: Headers { entrypoint: "...", includeDirs: [...], defines: [...] },
//!         sources: ["NativeLibs/Sokol/sokol_impl.c"],
//!         autobind: Autobind { module: "sokol", headers: [...], mode: AutobindMode.AllPublic },
//!         nativeTargets: [
//!             NativeTarget { triple: "aarch64-macos-none", staticLib: "...", frameworks: [...] }
//!         ],
//!     }
//! ]
//! ```
//!
//! This module decodes that form into the same
//! [`NativeLibrarySpec`] the TOML parser produces, so the two spellings are
//! interchangeable everywhere downstream. It lives beside
//! [`crate::declaration_loader`] rather than inside it because the record is
//! nested five levels deep and its reader is as long as the rest of the loader
//! put together.
//!
//! A field this reader does not know is ignored, matching the loader's rule; a
//! field it *does* know but cannot make sense of is an error, never a guess.

use kira_native_lib_definition::{
    AutobindMode, AutobindProfile, AutobindSpec, Availability, LinkMode, NativeArtifact,
    NativeHeaders, NativeLibrarySpec, NativeLinkAttributes, NativeTargetSpec, TargetTriple,
};

use crate::declaration_loader::{
    DeclarationError, array_items, malformed, non_empty_string, qualified_case, record_fields,
    string_value,
};

/// The manifest key this module reads, and the key every error names.
const KEY: &str = "nativeLibraries";

/// Reads the `nativeLibraries` array from a declaration.
pub(crate) fn native_libraries_value(
    value: &str,
) -> Result<Vec<NativeLibrarySpec>, DeclarationError> {
    let mut libraries = Vec::new();
    for item in array_items(KEY, value)? {
        libraries.push(library_value(item)?);
    }
    Ok(libraries)
}

/// Reads one `NativeLibrary { ... }` entry.
fn library_value(value: &str) -> Result<NativeLibrarySpec, DeclarationError> {
    let mut name = None;
    let mut link_mode = LinkMode::Static;
    let mut availability = Availability::Required;
    let mut headers = None;
    let mut sources = Vec::new();
    let mut autobind = None;
    let mut targets = Vec::new();

    for (field, value) in record_fields(KEY, "NativeLibrary", value)? {
        match field {
            "name" => {
                if name.is_some() {
                    return Err(malformed(KEY));
                }
                name = Some(non_empty_string(KEY, value)?);
            }
            "availability" => {
                availability =
                    Availability::parse(qualified_case(value)).ok_or_else(|| malformed(KEY))?;
            }
            "linkMode" => {
                link_mode = LinkMode::parse(qualified_case(value)).ok_or_else(|| malformed(KEY))?;
            }
            "headers" => headers = Some(headers_value(value)?),
            "sources" => sources = string_array(value)?,
            "autobind" => autobind = Some(autobind_value(value)?),
            "nativeTargets" => {
                for item in array_items(KEY, value)? {
                    targets.push(target_value(item)?);
                }
            }
            // A field this model does not carry yet is ignored, not rejected.
            _ => {}
        }
    }

    let name = name.ok_or_else(|| malformed(KEY))?;
    let mut spec =
        NativeLibrarySpec::new(name, link_mode, targets)?.with_availability(availability);
    if let Some(headers) = headers {
        spec = spec.with_headers(headers);
    }
    if !sources.is_empty() {
        spec = spec.with_sources(sources);
    }
    if let Some(autobind) = autobind {
        spec = spec.with_autobind(autobind);
    }
    Ok(spec)
}

/// Reads a `Headers { ... }` record.
fn headers_value(value: &str) -> Result<NativeHeaders, DeclarationError> {
    let mut headers = NativeHeaders::default();
    for (field, value) in record_fields(KEY, "Headers", value)? {
        match field {
            "entrypoint" => headers.entrypoint = Some(non_empty_string(KEY, value)?),
            "includeDirs" => headers.include_dirs = string_array(value)?,
            "defines" => headers.defines = string_array(value)?,
            _ => {}
        }
    }
    Ok(headers)
}

/// Reads an `Autobind { ... }` record.
fn autobind_value(value: &str) -> Result<AutobindSpec, DeclarationError> {
    let mut autobind = AutobindSpec::default();
    for (field, value) in record_fields(KEY, "Autobind", value)? {
        match field {
            "module" => autobind.module = Some(non_empty_string(KEY, value)?),
            "headers" => autobind.headers = string_array(value)?,
            "functions" => autobind.functions = string_array(value)?,
            "structs" => autobind.structs = string_array(value)?,
            "mode" => {
                autobind.mode =
                    AutobindMode::parse(qualified_case(value)).ok_or_else(|| malformed(KEY))?;
            }
            // The profile names a generator's own ruleset, so it is carried as
            // written rather than matched against a closed set this compiler
            // would have to grow for every new backend.
            "profile" => autobind.profile = Some(AutobindProfile::new(qualified_case(value))),
            "output" => autobind.output = Some(non_empty_string(KEY, value)?),
            _ => {}
        }
    }
    Ok(autobind)
}

/// Reads one `NativeTarget { ... }` row.
fn target_value(value: &str) -> Result<NativeTargetSpec, DeclarationError> {
    let mut triple = None;
    let mut static_lib = None;
    let mut dynamic_lib = None;
    let mut defines = Vec::new();
    let mut attributes = NativeLinkAttributes::default();

    for (field, value) in record_fields(KEY, "NativeTarget", value)? {
        match field {
            "triple" => triple = Some(TargetTriple::parse(&non_empty_string(KEY, value)?)?),
            // Both library paths may legitimately be empty (`dynamicLib: ""`
            // means "find it by install name"), so they read as plain strings
            // and `NativeArtifact::from_paths` decides what an empty one means.
            "staticLib" => static_lib = Some(string_value(KEY, value)?),
            "dynamicLib" => dynamic_lib = Some(string_value(KEY, value)?),
            "defines" => defines = string_array(value)?,
            "frameworks" => attributes.frameworks = string_array(value)?,
            "systemLibs" => attributes.system_libs = string_array(value)?,
            "compilerFlags" => attributes.compiler_flags = string_array(value)?,
            "linkerFlags" => attributes.linker_flags = string_array(value)?,
            _ => {}
        }
    }

    let triple = triple.ok_or_else(|| malformed(KEY))?;
    let artifact = NativeArtifact::from_paths(static_lib.as_deref(), dynamic_lib.as_deref());
    Ok(NativeTargetSpec::new(triple, artifact)
        .with_defines(defines)
        .with_attributes(attributes))
}

/// Reads a `["a", "b"]` array of quoted strings.
fn string_array(value: &str) -> Result<Vec<String>, DeclarationError> {
    array_items(KEY, value)?
        .into_iter()
        .map(|item| string_value(KEY, item))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::declaration_loader::load;
    use kira_native_lib_definition::NativeLibraryError;

    // The `kira-graphics` declaration, trimmed to two libraries: the full
    // sokol entry and the frameworks-only `kira_metal` one.
    const GRAPHICS: &str = r#"
Package KiraGraphics {
    let version = "0.1.0"
    let kind = PackageKind.Library
    let nativeLibraries = [
        NativeLibrary {
            name: "sokol",
            linkMode: LinkMode.Static,
            headers: Headers { entrypoint: "NativeLibs/Sokol/sokol_bindings.h", includeDirs: ["NativeLibs/Sokol"], defines: ["SOKOL_NO_ENTRY"] },
            sources: ["NativeLibs/Sokol/sokol_impl.c"],
            autobind: Autobind { module: "sokol", headers: ["NativeLibs/Sokol/sokol_app.h"], mode: AutobindMode.AllPublic },
            nativeTargets: [
                NativeTarget { triple: "aarch64-macos-none", staticLib: "generated/native/aarch64-macos/libsokol.a", defines: ["SOKOL_GLCORE"], frameworks: ["Foundation", "AppKit"] },
                NativeTarget { triple: "wasm32-emscripten-unknown", staticLib: "generated/native/wasm32-emscripten/libsokol.a", compilerFlags: ["--use-port=emdawnwebgpu"], linkerFlags: ["--use-port=emdawnwebgpu"] }
            ],
        },
        NativeLibrary {
            name: "kira_metal",
            linkMode: LinkMode.Dynamic,
            nativeTargets: [
                NativeTarget { triple: "aarch64-macos-none", frameworks: ["Metal", "AppKit"], systemLibs: ["objc"] }
            ],
        }
    ]
}
"#;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    #[test]
    fn reads_the_corpus_graphics_declaration() {
        let manifest = load(GRAPHICS).expect("a readable manifest");
        assert_eq!(manifest.native_libraries.len(), 2);

        let sokol = &manifest.native_libraries[0];
        assert_eq!(sokol.name(), "sokol");
        assert_eq!(sokol.link_mode(), LinkMode::Static);
        assert_eq!(sokol.sources(), ["NativeLibs/Sokol/sokol_impl.c"]);
        let headers = sokol.headers().expect("a headers record");
        assert_eq!(
            headers.entrypoint.as_deref(),
            Some("NativeLibs/Sokol/sokol_bindings.h")
        );
        assert_eq!(headers.include_dirs, ["NativeLibs/Sokol"]);
        let autobind = sokol.autobind().expect("an autobind record");
        assert_eq!(autobind.module.as_deref(), Some("sokol"));
        assert_eq!(autobind.mode, AutobindMode::AllPublic);

        assert_eq!(sokol.targets().len(), 2);
        let macos = &sokol.targets()[0];
        assert_eq!(macos.triple(), &triple("aarch64-macos-none"));
        assert_eq!(
            macos.artifact(),
            &NativeArtifact::StaticArchive("generated/native/aarch64-macos/libsokol.a".to_owned())
        );
        assert_eq!(macos.defines(), ["SOKOL_GLCORE"]);
        assert_eq!(macos.attributes().frameworks, ["Foundation", "AppKit"]);
        let wasm = &sokol.targets()[1];
        assert_eq!(
            wasm.attributes().compiler_flags,
            ["--use-port=emdawnwebgpu"]
        );
        assert_eq!(wasm.attributes().linker_flags, ["--use-port=emdawnwebgpu"]);
    }

    #[test]
    fn a_frameworks_only_library_needs_no_archive() {
        // `kira_metal` names no library file at all; dropping it would drop the
        // frameworks its symbols actually come from.
        let manifest = load(GRAPHICS).expect("a readable manifest");
        let metal = &manifest.native_libraries[1];
        assert_eq!(metal.link_mode(), LinkMode::Dynamic);
        assert_eq!(metal.targets()[0].artifact(), &NativeArtifact::None);
        assert_eq!(
            metal.targets()[0].attributes().frameworks,
            ["Metal", "AppKit"]
        );
        assert_eq!(metal.targets()[0].attributes().system_libs, ["objc"]);
    }

    #[test]
    fn an_empty_dynamic_lib_path_is_read_as_no_file() {
        let text = r#"
Package p {
    let nativeLibraries = [
        NativeLibrary {
            name: "vulkan",
            linkMode: LinkMode.Dynamic,
            nativeTargets: [
                NativeTarget { triple: "x86_64-linux-gnu", dynamicLib: "", systemLibs: ["vulkan"] }
            ],
        }
    ]
}
"#;
        let manifest = load(text).expect("a readable manifest");
        let row = &manifest.native_libraries[0].targets()[0];
        assert_eq!(row.artifact(), &NativeArtifact::None);
        assert_eq!(row.attributes().system_libs, ["vulkan"]);
    }

    #[test]
    fn a_manifest_with_no_native_libraries_declares_none() {
        let text = "Package p {\n let kind = .App\n}";
        assert!(
            load(text)
                .expect("a readable manifest")
                .native_libraries
                .is_empty()
        );
    }

    #[test]
    fn a_library_with_no_name_is_refused_by_key() {
        let text = r#"
Package p {
    let nativeLibraries = [
        NativeLibrary { linkMode: LinkMode.Static, nativeTargets: [] }
    ]
}
"#;
        assert_eq!(
            load(text).expect_err("a nameless library is refused"),
            DeclarationError::MalformedValue {
                key: KEY.to_owned()
            }
        );
    }

    #[test]
    fn a_malformed_triple_is_refused_by_name() {
        let text = r#"
Package p {
    let nativeLibraries = [
        NativeLibrary {
            name: "ffimath",
            nativeTargets: [NativeTarget { triple: "aarch64_macos", staticLib: "lib/a.a" }],
        }
    ]
}
"#;
        assert!(matches!(
            load(text).expect_err("a malformed triple is refused"),
            DeclarationError::Triple(_)
        ));
    }

    #[test]
    fn a_static_row_declaring_nothing_at_all_is_refused() {
        // Static and pathless says nothing about what to link; the same row
        // under `LinkMode.Dynamic` means "find it by name" and is accepted.
        let text = r#"
Package p {
    let nativeLibraries = [
        NativeLibrary {
            name: "ffimath",
            linkMode: LinkMode.Static,
            nativeTargets: [NativeTarget { triple: "aarch64-macos-none", staticLib: "" }],
        }
    ]
}
"#;
        assert!(matches!(
            load(text).expect_err("an empty static row is refused"),
            DeclarationError::InvalidNativeLibrary(error)
                if matches!(*error, NativeLibraryError::PathlessRow { .. })
        ));
    }
}
