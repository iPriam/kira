//! Parse and render the legacy `kira.toml` project manifest.
//!
//! `package.kira` is the canonical declaration format, but older packages use
//! a conventional TOML document. This module accepts both the common
//! `[package]` shape and a flat document, preserves path/registry/git
//! dependencies, and emits a deterministic TOML form suitable for migration.
//! The canonical declaration writer remains separate because the two formats
//! have different grammars and different native-library representations.

use std::path::{Path, PathBuf};

use toml::{Table, Value};

use crate::dependency::{DependencySource, DependencySpec, GitSource, PathSource, RegistrySource};
use crate::platform_config::ExecutionPolicy;
use crate::project_manifest::{PackageKind, ProjectManifest};
use crate::toml_text::{quoted, quoted_array, string, strings, table};

/// Why a legacy TOML manifest could not be decoded or rendered.
#[derive(Debug, thiserror::Error)]
pub enum LegacyManifestError {
    /// The text is not valid TOML.
    #[error("malformed TOML manifest: {0}")]
    Toml(#[from] toml::de::Error),
    /// A required project field is absent.
    #[error("legacy manifest is missing `{field}`")]
    MissingField {
        /// The missing field name.
        field: &'static str,
    },
    /// A known field has the wrong TOML type or value.
    #[error("legacy manifest field `{field}` is invalid: {message}")]
    InvalidField {
        /// The field being decoded.
        field: String,
        /// What was wrong with it.
        message: String,
    },
    /// The legacy grammar cannot represent a newer model field without
    /// silently losing information.
    #[error("legacy TOML cannot represent manifest field `{field}`")]
    UnsupportedField {
        /// The model field that would be dropped.
        field: &'static str,
    },
    /// The destination file could not be written.
    #[error("legacy manifest `{path}` could not be written")]
    Write {
        /// The destination path.
        path: PathBuf,
        /// The filesystem failure.
        #[source]
        source: std::io::Error,
    },
}

/// Parses a legacy TOML project manifest into the shared project model.
pub fn load_legacy_manifest(text: &str) -> Result<ProjectManifest, LegacyManifestError> {
    let document: Table = toml::from_str(text)?;
    let package = table(&document, "package").unwrap_or(&document);
    let name = string(package, "name")
        .or_else(|| string(&document, "name"))
        .ok_or(LegacyManifestError::MissingField { field: "name" })?;
    let version = string(package, "version")
        .or_else(|| string(&document, "version"))
        .unwrap_or("0.1.0");
    let mut manifest = ProjectManifest::new(name, version);
    manifest.kira_version = string(package, "kira")
        .or_else(|| string(package, "kira_version"))
        .or_else(|| string(&document, "kira"))
        .or_else(|| string(&document, "kira_version"))
        .unwrap_or("0.1.0")
        .to_owned();
    if let Some(kind) = string(package, "kind").or_else(|| string(&document, "kind")) {
        manifest.kind =
            PackageKind::parse(kind).ok_or_else(|| LegacyManifestError::InvalidField {
                field: "kind".to_owned(),
                message: format!("`{kind}` is not `app` or `library`"),
            })?;
    }
    manifest.module_root = string(package, "module_root")
        .or_else(|| string(package, "moduleRoot"))
        .or_else(|| string(&document, "module_root"))
        .or_else(|| string(&document, "moduleRoot"))
        .map(ToOwned::to_owned);
    manifest.assets = strings(package, "assets").map_err(|message| invalid("assets", message))?;
    if manifest.assets.is_empty() && !package.contains_key("assets") {
        manifest.assets =
            strings(&document, "assets").map_err(|message| invalid("assets", message))?;
    }
    manifest.packages =
        strings(package, "packages").map_err(|message| invalid("packages", message))?;
    if manifest.packages.is_empty() && !package.contains_key("packages") {
        manifest.packages =
            strings(&document, "packages").map_err(|message| invalid("packages", message))?;
    }
    manifest.dependencies = parse_dependencies(
        package
            .get("dependencies")
            .or_else(|| document.get("dependencies")),
    )?;
    manifest.execution_mode = string(package, "execution_mode")
        .or_else(|| string(package, "executionMode"))
        .or_else(|| string(&document, "execution_mode"))
        .or_else(|| string(&document, "executionMode"))
        .unwrap_or("vm")
        .to_owned();
    if !matches!(manifest.execution_mode.as_str(), "vm" | "llvm" | "hybrid") {
        return Err(invalid(
            "execution_mode",
            format!("unsupported backend `{}`", manifest.execution_mode),
        ));
    }
    manifest.build_target = string(package, "build_target")
        .or_else(|| string(package, "buildTarget"))
        .or_else(|| string(&document, "build_target"))
        .or_else(|| string(&document, "buildTarget"))
        .unwrap_or("host")
        .to_owned();
    if !matches!(manifest.build_target.as_str(), "host" | "wasm32" | "wasm64") {
        return Err(invalid(
            "build_target",
            format!("unsupported target `{}`", manifest.build_target),
        ));
    }
    if let Some(registry) = table(&document, "registry") {
        manifest.registry_url = string(registry, "url").map(ToOwned::to_owned);
        manifest.registry_token_env = string(registry, "token_env")
            .or_else(|| string(registry, "tokenEnv"))
            .map(ToOwned::to_owned);
    }
    Ok(manifest)
}

/// Renders the model fields represented by the legacy TOML schema.
pub fn render_legacy_manifest(manifest: &ProjectManifest) -> Result<String, LegacyManifestError> {
    if !manifest.native_libraries.is_empty() {
        return Err(LegacyManifestError::UnsupportedField {
            field: "native_libraries",
        });
    }
    if manifest.tests.is_some() {
        return Err(LegacyManifestError::UnsupportedField { field: "tests" });
    }
    if manifest.execution_policy != ExecutionPolicy::default() {
        return Err(LegacyManifestError::UnsupportedField {
            field: "execution_policy",
        });
    }
    if !matches!(manifest.execution_mode.as_str(), "vm" | "llvm" | "hybrid") {
        return Err(invalid(
            "execution_mode",
            format!("unsupported backend `{}`", manifest.execution_mode),
        ));
    }
    if !matches!(manifest.build_target.as_str(), "host" | "wasm32" | "wasm64") {
        return Err(invalid(
            "build_target",
            format!("unsupported target `{}`", manifest.build_target),
        ));
    }

    let mut text = String::from("[package]\n");
    push_string(&mut text, "name", &manifest.name);
    push_string(&mut text, "version", &manifest.version);
    push_string(&mut text, "kind", manifest.kind.label());
    push_string(&mut text, "kira", &manifest.kira_version);
    if let Some(module_root) = &manifest.module_root {
        push_string(&mut text, "module_root", module_root);
    }
    if !manifest.assets.is_empty() {
        text.push_str("assets = ");
        text.push_str(&quoted_array(&manifest.assets));
        text.push('\n');
    }
    if !manifest.packages.is_empty() {
        text.push_str("packages = ");
        text.push_str(&quoted_array(&manifest.packages));
        text.push('\n');
    }
    push_string(&mut text, "execution_mode", &manifest.execution_mode);
    push_string(&mut text, "build_target", &manifest.build_target);
    if !manifest.dependencies.is_empty() {
        text.push_str("\n[dependencies]\n");
        for dependency in &manifest.dependencies {
            text.push_str(&quoted(&dependency.name));
            text.push_str(" = ");
            push_dependency_value(&mut text, dependency);
            text.push('\n');
        }
    }
    if manifest.registry_url.is_some() || manifest.registry_token_env.is_some() {
        text.push_str("\n[registry]\n");
        if let Some(url) = &manifest.registry_url {
            push_string(&mut text, "url", url);
        }
        if let Some(token_env) = &manifest.registry_token_env {
            push_string(&mut text, "token_env", token_env);
        }
    }
    Ok(text)
}

/// Writes a legacy TOML project manifest.
pub fn write_legacy_manifest(
    path: impl AsRef<Path>,
    manifest: &ProjectManifest,
) -> Result<(), LegacyManifestError> {
    let path = path.as_ref().to_path_buf();
    let text = render_legacy_manifest(manifest)?;
    std::fs::write(&path, text).map_err(|source| LegacyManifestError::Write { path, source })
}

fn parse_dependencies(value: Option<&Value>) -> Result<Vec<DependencySpec>, LegacyManifestError> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    if let Some(table) = value.as_table() {
        return table
            .iter()
            .map(|(name, value)| parse_dependency(name, value))
            .collect();
    }
    if let Some(rows) = value.as_array() {
        return rows
            .iter()
            .map(|value| {
                let row = value.as_table().ok_or_else(|| {
                    invalid("dependencies", "array entries must be tables".to_owned())
                })?;
                let name = row.get("name").and_then(Value::as_str).ok_or(
                    LegacyManifestError::MissingField {
                        field: "dependencies.name",
                    },
                )?;
                parse_dependency(name, value)
            })
            .collect();
    }
    Err(invalid(
        "dependencies",
        "expected a table or array of tables".to_owned(),
    ))
}

fn parse_dependency(name: &str, value: &Value) -> Result<DependencySpec, LegacyManifestError> {
    let table = match value {
        Value::String(version) => {
            return Ok(DependencySpec {
                name: name.to_owned(),
                source: DependencySource::Registry(RegistrySource {
                    version: version.clone(),
                }),
            });
        }
        Value::Table(table) => table,
        _ => {
            return Err(invalid(
                "dependencies",
                "dependency must be a string or table".to_owned(),
            ));
        }
    };
    let path = table.get("path").and_then(Value::as_str);
    let version = table.get("version").and_then(Value::as_str);
    let url = table
        .get("git")
        .or_else(|| table.get("url"))
        .and_then(Value::as_str);
    let source = match (path, version, url) {
        (Some(path), None, None) => DependencySource::Path(PathSource {
            path: path.to_owned(),
        }),
        (None, Some(version), None) => DependencySource::Registry(RegistrySource {
            version: version.to_owned(),
        }),
        (None, None, Some(url)) => DependencySource::Git(GitSource {
            url: url.to_owned(),
            rev: table
                .get("rev")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            tag: table
                .get("tag")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        }),
        _ => {
            return Err(invalid(
                "dependencies",
                format!("dependency `{name}` must have exactly one path, version, or git source"),
            ));
        }
    };
    Ok(DependencySpec {
        name: name.to_owned(),
        source,
    })
}

fn push_dependency_value(text: &mut String, dependency: &DependencySpec) {
    match &dependency.source {
        DependencySource::Path(source) => {
            text.push_str("{ path = ");
            text.push_str(&quoted(&source.path));
            text.push_str(" }");
        }
        DependencySource::Registry(source) => {
            text.push_str("{ version = ");
            text.push_str(&quoted(&source.version));
            text.push_str(" }");
        }
        DependencySource::Git(source) => {
            text.push_str("{ git = ");
            text.push_str(&quoted(&source.url));
            if let Some(rev) = &source.rev {
                text.push_str(", rev = ");
                text.push_str(&quoted(rev));
            }
            if let Some(tag) = &source.tag {
                text.push_str(", tag = ");
                text.push_str(&quoted(tag));
            }
            text.push_str(" }");
        }
    }
}

fn push_string(text: &mut String, key: &str, value: &str) {
    text.push_str(key);
    text.push_str(" = ");
    text.push_str(&quoted(value));
    text.push('\n');
}

fn invalid(field: &'static str, message: String) -> LegacyManifestError {
    LegacyManifestError::InvalidField {
        field: field.to_owned(),
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_manifest_supports_flat_and_sectioned_documents() {
        let flat = r#"
name = "Flat"
version = "0.2.0"
kind = "library"
module_root = "Flat"
assets = ["Assets"]
[dependencies]
Core = { path = "../core" }
Registry = { version = "1.2.3" }
Git = { git = "https://example.test/repo.git", tag = "v1" }
"#;
        let manifest = load_legacy_manifest(flat).expect("legacy manifest");
        assert_eq!(manifest.name, "Flat");
        assert_eq!(manifest.kind, PackageKind::Library);
        assert_eq!(manifest.dependencies.len(), 3);
        assert_eq!(manifest.assets, ["Assets"]);

        let sectioned = r#"
[package]
name = "Sectioned"
version = "1.0.0"
kind = "app"
kira = "1.0.0"
execution_mode = "llvm"
build_target = "wasm32"
[registry]
url = "https://registry.example"
token_env = "KIRA_TOKEN"
"#;
        let manifest = load_legacy_manifest(sectioned).expect("legacy manifest");
        assert_eq!(manifest.execution_mode, "llvm");
        assert_eq!(manifest.build_target, "wasm32");
        assert_eq!(
            manifest.registry_url.as_deref(),
            Some("https://registry.example")
        );
    }

    #[test]
    fn legacy_render_round_trips_and_refuses_unrepresentable_fields() {
        let mut manifest = ProjectManifest::new("Demo", "1.0.0");
        manifest.dependencies.push(DependencySpec {
            name: "Core".to_owned(),
            source: DependencySource::Path(PathSource {
                path: "../core".to_owned(),
            }),
        });
        manifest.assets = vec!["data\"set".to_owned()];
        let text = render_legacy_manifest(&manifest).expect("render");
        let loaded = load_legacy_manifest(&text).expect("round trip");
        assert_eq!(loaded.name, manifest.name);
        assert_eq!(loaded.dependencies, manifest.dependencies);
        assert_eq!(loaded.assets, manifest.assets);

        manifest.tests = Some(crate::TestsConfig {
            backends: vec![crate::Backend::Vm],
            phase: crate::TestPhase::Run,
        });
        assert!(matches!(
            render_legacy_manifest(&manifest),
            Err(LegacyManifestError::UnsupportedField { field: "tests" })
        ));
    }
}
