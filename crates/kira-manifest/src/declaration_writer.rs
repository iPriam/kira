//! Render a [`ProjectManifest`] as a deterministic `package.kira` declaration.
//!
//! The declaration reader is intentionally small and hand-written, so the
//! writer mirrors its grammar instead of routing configuration through the
//! Kira source parser. Every field the declaration reader understands is
//! emitted here, in a stable order. Resolved build matrices are derived state
//! and are not serialized.

use std::path::{Path, PathBuf};

use kira_native_lib_definition::{
    AutobindMode, NativeArtifact, NativeLibrarySpec, NativeTargetSpec,
};

use crate::dependency::{DependencySource, DependencySpec};
use crate::project_manifest::{PackageKind, ProjectManifest};

/// Why a declaration could not be rendered or written.
#[derive(Debug, thiserror::Error)]
pub enum DeclarationWriteError {
    /// The package name cannot be placed safely after `Package`.
    #[error("package name `{name}` cannot be written in a declaration header")]
    InvalidPackageName {
        /// The name supplied by the manifest.
        name: String,
    },
    /// A manifest value names an execution mode or target the declaration
    /// grammar does not know.
    #[error("manifest field `{field}` has unsupported value `{value}`")]
    UnsupportedValue {
        /// The field containing the value.
        field: &'static str,
        /// The value that could not be mapped to a declaration case.
        value: String,
    },
    /// The destination file could not be written.
    #[error("package declaration `{path}` could not be written")]
    Write {
        /// The destination path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

/// Renders a project manifest into `package.kira` text.
///
/// The output is accepted by [`crate::load_declaration`] and is stable for a
/// stable manifest: dependency and native-library order is preserved because
/// that order is useful to a human reviewing a declaration, while fields have
/// one canonical spelling and indentation.
pub fn render(manifest: &ProjectManifest) -> Result<String, DeclarationWriteError> {
    validate_package_name(&manifest.name)?;
    let execution_mode = backend_case(&manifest.execution_mode)?;
    let build_target = target_case(&manifest.build_target)?;

    let mut text = format!("Package {} {{\n", manifest.name);
    push_string_field(&mut text, 1, "version", &manifest.version);
    push_string_field(&mut text, 1, "kira", &manifest.kira_version);
    push_case_field(&mut text, 1, "kind", package_kind_case(manifest.kind));
    if let Some(module_root) = &manifest.module_root {
        push_string_field(&mut text, 1, "moduleRoot", module_root);
    }
    push_string_array_field(&mut text, 1, "assets", &manifest.assets);
    push_string_array_field(&mut text, 1, "packages", &manifest.packages);
    if manifest.allow_thin_ffi_shim {
        push_bool_field(&mut text, 1, "allowThinFfiShim", true);
    }
    push_dependencies(&mut text, &manifest.dependencies);
    push_native_libraries(&mut text, &manifest.native_libraries);
    push_defaults(&mut text, execution_mode, build_target);
    text.push_str("}\n");
    Ok(text)
}

/// Writes a project manifest to `path`.
pub fn write(
    path: impl AsRef<Path>,
    manifest: &ProjectManifest,
) -> Result<(), DeclarationWriteError> {
    let path = path.as_ref().to_path_buf();
    let text = render(manifest)?;
    std::fs::write(&path, text).map_err(|source| DeclarationWriteError::Write { path, source })
}

fn validate_package_name(name: &str) -> Result<(), DeclarationWriteError> {
    if name.is_empty()
        || name
            .chars()
            .any(|ch| ch.is_whitespace() || matches!(ch, '{' | '}'))
    {
        return Err(DeclarationWriteError::InvalidPackageName {
            name: name.to_owned(),
        });
    }
    Ok(())
}

fn backend_case(value: &str) -> Result<&'static str, DeclarationWriteError> {
    match value {
        "vm" => Ok("Vm"),
        "llvm" => Ok("Llvm"),
        "hybrid" => Ok("Hybrid"),
        _ => Err(DeclarationWriteError::UnsupportedValue {
            field: "executionMode",
            value: value.to_owned(),
        }),
    }
}

fn target_case(value: &str) -> Result<&'static str, DeclarationWriteError> {
    match value {
        "host" => Ok("Host"),
        "wasm32" => Ok("Wasm32"),
        "wasm64" => Ok("Wasm64"),
        _ => Err(DeclarationWriteError::UnsupportedValue {
            field: "buildTarget",
            value: value.to_owned(),
        }),
    }
}

fn push_string_field(text: &mut String, depth: usize, key: &str, value: &str) {
    indent(text, depth);
    text.push_str("let ");
    text.push_str(key);
    text.push_str(" = ");
    push_quoted(text, value);
    text.push('\n');
}

fn push_case_field(text: &mut String, depth: usize, key: &str, value: &str) {
    indent(text, depth);
    text.push_str("let ");
    text.push_str(key);
    text.push_str(" = .");
    text.push_str(value);
    text.push('\n');
}

fn push_bool_field(text: &mut String, depth: usize, key: &str, value: bool) {
    indent(text, depth);
    text.push_str("let ");
    text.push_str(key);
    text.push_str(" = ");
    text.push_str(if value { "true" } else { "false" });
    text.push('\n');
}

fn push_string_array_field(text: &mut String, depth: usize, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    indent(text, depth);
    text.push_str("let ");
    text.push_str(key);
    text.push_str(" = [");
    push_quoted_list(text, values);
    text.push_str("]\n");
}

fn push_dependencies(text: &mut String, dependencies: &[DependencySpec]) {
    if dependencies.is_empty() {
        return;
    }
    indent(text, 1);
    text.push_str("let dependencies = [\n");
    for dependency in dependencies {
        indent(text, 2);
        text.push_str("Dependency { name: ");
        push_quoted(text, &dependency.name);
        match &dependency.source {
            DependencySource::Registry(source) => {
                text.push_str(", version: ");
                push_quoted(text, &source.version);
            }
            DependencySource::Path(source) => {
                text.push_str(", path: ");
                push_quoted(text, &source.path);
            }
            DependencySource::Git(source) => {
                text.push_str(", url: ");
                push_quoted(text, &source.url);
                if let Some(rev) = &source.rev {
                    text.push_str(", rev: ");
                    push_quoted(text, rev);
                }
                if let Some(tag) = &source.tag {
                    text.push_str(", tag: ");
                    push_quoted(text, tag);
                }
            }
        }
        text.push_str(" },\n");
    }
    indent(text, 1);
    text.push_str("]\n");
}

fn push_defaults(text: &mut String, execution_mode: &str, build_target: &str) {
    indent(text, 1);
    text.push_str("let defaults = Defaults { executionMode: .");
    text.push_str(execution_mode);
    text.push_str(", buildTarget: .");
    text.push_str(build_target);
    text.push_str(" }\n");
}

fn push_native_libraries(text: &mut String, libraries: &[NativeLibrarySpec]) {
    if libraries.is_empty() {
        return;
    }
    indent(text, 1);
    text.push_str("let nativeLibraries = [\n");
    for library in libraries {
        push_native_library(text, library);
    }
    indent(text, 1);
    text.push_str("]\n");
}

fn push_native_library(text: &mut String, library: &NativeLibrarySpec) {
    indent(text, 2);
    text.push_str("NativeLibrary {\n");
    push_record_string_field(text, 3, "name", library.name());
    push_record_case_field(text, 3, "linkMode", case_name(library.link_mode().label()));
    if library.availability().label() != "required" {
        push_record_case_field(
            text,
            3,
            "availability",
            case_name(library.availability().label()),
        );
    }
    if let Some(headers) = library.headers() {
        indent(text, 3);
        text.push_str("headers: Headers { ");
        let mut first = true;
        if let Some(entrypoint) = &headers.entrypoint {
            inline_separator(text, &mut first);
            text.push_str("entrypoint: ");
            push_quoted(text, entrypoint);
        }
        if !headers.include_dirs.is_empty() {
            inline_separator(text, &mut first);
            push_inline_string_array(text, "includeDirs", &headers.include_dirs);
        }
        if !headers.defines.is_empty() {
            inline_separator(text, &mut first);
            push_inline_string_array(text, "defines", &headers.defines);
        }
        text.push_str(" },\n");
    }
    push_record_string_array_field(text, 3, "sources", library.sources());
    if let Some(autobind) = library.autobind() {
        indent(text, 3);
        text.push_str("autobind: Autobind { ");
        let mut first = true;
        if let Some(module) = &autobind.module {
            inline_separator(text, &mut first);
            text.push_str("module: ");
            push_quoted(text, module);
        }
        if !autobind.headers.is_empty() {
            inline_separator(text, &mut first);
            push_inline_string_array(text, "headers", &autobind.headers);
        }
        if !autobind.functions.is_empty() {
            inline_separator(text, &mut first);
            push_inline_string_array(text, "functions", &autobind.functions);
        }
        if !autobind.structs.is_empty() {
            inline_separator(text, &mut first);
            push_inline_string_array(text, "structs", &autobind.structs);
        }
        inline_separator(text, &mut first);
        text.push_str("mode: .");
        text.push_str(match autobind.mode {
            AutobindMode::AllPublic => "AllPublic",
            AutobindMode::Selected => "Selected",
        });
        if let Some(output) = &autobind.output {
            inline_separator(text, &mut first);
            text.push_str("output: ");
            push_quoted(text, output);
        }
        text.push_str(" },\n");
    }
    if !library.targets().is_empty() {
        indent(text, 3);
        text.push_str("nativeTargets: [\n");
        for target in library.targets() {
            push_native_target(text, target);
        }
        indent(text, 3);
        text.push_str("],\n");
    }
    indent(text, 2);
    text.push_str("},\n");
}

fn push_native_target(text: &mut String, target: &NativeTargetSpec) {
    indent(text, 4);
    text.push_str("NativeTarget { triple: ");
    push_quoted(text, &target.triple().to_string());
    if let Some(path) = target.artifact().path() {
        match target.artifact() {
            NativeArtifact::StaticArchive(_) => text.push_str(", staticLib: "),
            NativeArtifact::SharedLibrary(_) => text.push_str(", dynamicLib: "),
            NativeArtifact::None => unreachable!("artifact path has no None case"),
        }
        push_quoted(text, path);
    }
    push_inline_array_if_non_empty(text, "defines", target.defines());
    let attributes = target.attributes();
    push_inline_array_if_non_empty(text, "frameworks", &attributes.frameworks);
    push_inline_array_if_non_empty(text, "systemLibs", &attributes.system_libs);
    push_inline_array_if_non_empty(text, "compilerFlags", &attributes.compiler_flags);
    push_inline_array_if_non_empty(text, "linkerFlags", &attributes.linker_flags);
    push_inline_array_if_non_empty(text, "runtimeFiles", &attributes.runtime_files);
    text.push_str(" },\n");
}

fn push_record_string_field(text: &mut String, depth: usize, key: &str, value: &str) {
    indent(text, depth);
    text.push_str(key);
    text.push_str(": ");
    push_quoted(text, value);
    text.push_str(",\n");
}

fn push_record_case_field(text: &mut String, depth: usize, key: &str, value: &str) {
    indent(text, depth);
    text.push_str(key);
    text.push_str(": .");
    text.push_str(value);
    text.push_str(",\n");
}

fn push_record_string_array_field(text: &mut String, depth: usize, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    indent(text, depth);
    text.push_str(key);
    text.push_str(": [");
    push_quoted_list(text, values);
    text.push_str("],\n");
}

fn push_inline_array_if_non_empty(text: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    text.push_str(", ");
    push_inline_string_array(text, key, values);
}

fn push_inline_string_array(text: &mut String, key: &str, values: &[String]) {
    if values.is_empty() {
        return;
    }
    text.push_str(key);
    text.push_str(": [");
    push_quoted_list(text, values);
    text.push(']');
}

fn inline_separator(text: &mut String, first: &mut bool) {
    if !*first {
        text.push_str(", ");
    }
    *first = false;
}

fn push_quoted_list(text: &mut String, values: &[String]) {
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            text.push_str(", ");
        }
        push_quoted(text, value);
    }
}

fn push_quoted(text: &mut String, value: &str) {
    text.push('"');
    for ch in value.chars() {
        match ch {
            '\\' => text.push_str("\\\\"),
            '"' => text.push_str("\\\""),
            '\n' => text.push_str("\\n"),
            '\r' => text.push_str("\\r"),
            '\t' => text.push_str("\\t"),
            '\0' => text.push_str("\\0"),
            _ => text.push(ch),
        }
    }
    text.push('"');
}

fn indent(text: &mut String, depth: usize) {
    text.push_str(&"    ".repeat(depth));
}

/// The declaration spelling is the capitalized case name, while the model's
/// labels are deliberately lowercase for diagnostics and TOML.
fn case_name(label: &str) -> &'static str {
    match label {
        "static" => "Static",
        "dynamic" => "Dynamic",
        "runtime" => "Runtime",
        "required" => "Required",
        "optional" => "Optional",
        _ => unreachable!("all native declaration cases have a known label"),
    }
}

fn package_kind_case(kind: PackageKind) -> &'static str {
    match kind {
        PackageKind::App => "App",
        PackageKind::Library => "Library",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_native_lib_definition::{
        LinkMode, NativeArtifact, NativeHeaders, NativeLinkAttributes, NativeTargetSpec,
        TargetTriple,
    };

    fn triple(value: &str) -> TargetTriple {
        TargetTriple::parse(value).expect("valid target triple")
    }

    #[test]
    fn a_manifest_renders_and_loads_with_all_authored_fields() {
        let native = NativeLibrarySpec::new(
            "demo",
            LinkMode::Dynamic,
            vec![
                NativeTargetSpec::new(
                    triple("x86_64-windows-msvc"),
                    NativeArtifact::SharedLibrary("lib/demo.dll".to_owned()),
                )
                .with_defines(vec!["DEMO".to_owned()])
                .with_attributes(NativeLinkAttributes {
                    system_libs: vec!["user32".to_owned()],
                    runtime_files: vec!["bin/demo.dll".to_owned()],
                    ..NativeLinkAttributes::default()
                }),
            ],
        )
        .expect("valid native library")
        .with_headers(NativeHeaders {
            entrypoint: Some("include/demo.h".to_owned()),
            include_dirs: vec!["include".to_owned()],
            defines: vec!["DEMO_HEADER".to_owned()],
        })
        .with_sources(vec!["src/demo.c".to_owned()]);

        let mut manifest = ProjectManifest::new("Demo", "1.2.3");
        manifest.kira_version = "1.0.0".to_owned();
        manifest.kind = PackageKind::Library;
        manifest.module_root = Some("Demo".to_owned());
        manifest.assets = vec!["Assets".to_owned(), "data\\demo\".bin".to_owned()];
        manifest.packages = vec!["app".to_owned()];
        manifest.dependencies = vec![
            DependencySpec {
                name: "Core".to_owned(),
                source: DependencySource::Path(crate::dependency::PathSource {
                    path: "../core".to_owned(),
                }),
            },
            DependencySpec {
                name: "Remote".to_owned(),
                source: DependencySource::Git(crate::dependency::GitSource {
                    url: "https://example.test/a.git".to_owned(),
                    rev: Some("abc".to_owned()),
                    tag: None,
                }),
            },
        ];
        manifest.native_libraries = vec![native];
        manifest.execution_mode = "llvm".to_owned();
        manifest.build_target = "wasm32".to_owned();

        let text = render(&manifest).expect("render");
        let loaded = crate::load_declaration(&text).expect("writer output loads");
        assert_eq!(loaded.name, manifest.name);
        assert_eq!(loaded.version, manifest.version);
        assert_eq!(loaded.kira_version, manifest.kira_version);
        assert_eq!(loaded.kind, manifest.kind);
        assert_eq!(loaded.module_root, manifest.module_root);
        assert_eq!(loaded.assets, manifest.assets);
        assert_eq!(loaded.packages, manifest.packages);
        assert_eq!(loaded.dependencies, manifest.dependencies);
        assert_eq!(loaded.native_libraries, manifest.native_libraries);
        assert_eq!(loaded.execution_mode, manifest.execution_mode);
        assert_eq!(loaded.build_target, manifest.build_target);
    }

    #[test]
    fn invalid_header_names_and_defaults_are_rejected() {
        let mut manifest = ProjectManifest::new("bad name", "0.1.0");
        assert!(matches!(
            render(&manifest),
            Err(DeclarationWriteError::InvalidPackageName { .. })
        ));

        manifest.name = "ok".to_owned();
        manifest.execution_mode = "native".to_owned();
        assert!(matches!(
            render(&manifest),
            Err(DeclarationWriteError::UnsupportedValue {
                field: "executionMode",
                ..
            })
        ));
    }
}
