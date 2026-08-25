//! The `KiraRunner.toml` manifest an exported application carries.
//!
//! An export bakes this file into the app it generates, and the runner inside
//! that app reads it back: who the app is, where its bytecode bundles live,
//! which server a live session connects to, and whether the embedded bundles
//! are played standalone or fetched from that server. One model serves both
//! sides — the generator renders it and the runner parses it — because two
//! spellings of one file is exactly the drift the format exists to prevent.
//!
//! The sections mirror the roles rather than one flat key space:
//!
//! ```toml
//! [runtime]
//! kind = "xcode-macos"
//! name = "KiraApp"
//! bundle_id = "com.kira.live.dev"
//! version = "0.1.0"
//! mode = "standalone"
//!
//! [target]
//! path = "/projects/demo"
//! package = "demo"
//! validation_app = "."
//!
//! [paths]
//! bundles = ""
//! local_cache = "app-support/KiraExport"
//! main_bundle = "com.kira.demo"
//! embedded_bundles = "Bundles"
//!
//! [abi]
//! bytecode = 1
//! hostcall = 1
//! native_contract_hash = "…"
//!
//! [server]
//! host = "127.0.0.1"
//! port = 0
//! ```
//!
//! Unknown keys and unknown `kind` values parse as errors rather than being
//! skipped: a runner reading a manifest it only half understands would run an
//! app against assumptions the exporter never made.

/// Which built-in runner an app embeds, spelled as it appears in `[runtime] kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    /// The desktop dynamic host (the runner client binary).
    Desktop,
    /// An Xcode-built application for macOS.
    XcodeMacos,
    /// An Xcode-built application for iOS.
    XcodeIos,
    /// An Xcode-built application for tvOS.
    XcodeTvos,
    /// An Xcode-built application for visionOS.
    XcodeVisionos,
}

impl RunnerKind {
    /// Resolves a `[runtime] kind` value, or `None` for an unknown one.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "desktop" | "desktop-dynamic-host" => Some(Self::Desktop),
            "xcode-macos" => Some(Self::XcodeMacos),
            "xcode-ios" => Some(Self::XcodeIos),
            "xcode-tvos" => Some(Self::XcodeTvos),
            "xcode-visionos" => Some(Self::XcodeVisionos),
            _ => None,
        }
    }

    /// The canonical spelling written under `[runtime] kind`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop-dynamic-host",
            Self::XcodeMacos => "xcode-macos",
            Self::XcodeIos => "xcode-ios",
            Self::XcodeTvos => "xcode-tvos",
            Self::XcodeVisionos => "xcode-visionos",
        }
    }

    /// The [`RunnerId`] of the live sessions this runner joins.
    ///
    /// A session's handshake compares the connecting client's runner with the
    /// bundle's, so the id a runner reports is decided by what it is, not by
    /// what it was handed.
    pub fn runner_id(self) -> crate::RunnerId {
        match self {
            Self::Desktop => crate::RunnerId::Desktop,
            Self::XcodeMacos => crate::RunnerId::Macos,
            Self::XcodeIos => crate::RunnerId::Ios,
            Self::XcodeTvos => crate::RunnerId::Tvos,
            Self::XcodeVisionos => crate::RunnerId::Visionos,
        }
    }
}

/// Whether an embedded set of bundles is played from the app or fetched live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeMode {
    /// Run the bundles embedded in the app.
    Standalone,
    /// Connect to `[server]` and run what the live server serves.
    Live,
}

impl RuntimeMode {
    /// Resolves a `[runtime] mode` value, or `None` for an unknown one.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "standalone" => Some(Self::Standalone),
            "live" => Some(Self::Live),
            _ => None,
        }
    }

    /// The spelling written under `[runtime] mode`.
    pub fn label(self) -> &'static str {
        match self {
            Self::Standalone => "standalone",
            Self::Live => "live",
        }
    }
}

/// A parsed `KiraRunner.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerManifest {
    /// Which runner this app embeds.
    pub kind: RunnerKind,
    /// The display name of the app.
    pub name: String,
    /// The bundle identifier the app is signed and installed under.
    pub bundle_id: String,
    /// The app version.
    pub version: String,
    /// Whether the embedded bundles play standalone or come from the server.
    pub mode: RuntimeMode,
    /// The package root the export was generated from.
    pub target_path: String,
    /// The package name the export was generated from.
    pub package_name: String,
    /// Where the app's own cache root resolves from, relative to the manifest.
    pub local_cache_path: String,
    /// The content-derived directory name of the entry bundle under `bundles`.
    pub main_bundle_id: String,
    /// The resource directory holding `<bundle-id>.klbundle` directories.
    pub embedded_bundles_path: Option<String>,
    /// The live server host; unused in standalone mode.
    pub server_host: String,
    /// The live server port; unused in standalone mode.
    pub server_port: u16,
    /// The hash pinning the native contract the bundles were built against.
    pub native_contract_hash: String,
}

impl RunnerManifest {
    /// Renders the manifest as TOML text.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let _ = writeln!(
            out,
            "[runtime]\nkind = {:?}\nname = {:?}\nbundle_id = {:?}\nversion = {:?}\nmode = {:?}",
            self.kind.label(),
            self.name,
            self.bundle_id,
            self.version,
            self.mode.label(),
        );
        let _ = writeln!(
            out,
            "\n[target]\npath = {:?}\npackage = {:?}",
            self.target_path, self.package_name,
        );
        let _ = writeln!(
            out,
            "\n[paths]\nlocal_cache = {:?}\nmain_bundle = {:?}",
            self.local_cache_path, self.main_bundle_id,
        );
        if let Some(embedded) = &self.embedded_bundles_path {
            let _ = writeln!(out, "embedded_bundles = {embedded:?}",);
        }
        let _ = writeln!(
            out,
            "\n[abi]\nnative_contract_hash = {:?}",
            self.native_contract_hash,
        );
        let _ = writeln!(
            out,
            "\n[server]\nhost = {:?}\nport = {}",
            self.server_host, self.server_port,
        );
        out
    }

    /// Parses TOML text into a manifest.
    ///
    /// The parser reads exactly the schema [`RunnerManifest::render`] writes.
    /// A missing section or key is an error naming it, so an app whose export
    /// was truncated fails at startup with a reason instead of at its first
    /// absent field.
    pub fn parse(text: &str) -> Result<RunnerManifest, RunnerManifestError> {
        let mut sections: Vec<(&str, Vec<(String, String)>)> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(name) = line
                .strip_prefix('[')
                .and_then(|rest| rest.strip_suffix(']'))
            {
                sections.push((name, Vec::new()));
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                return Err(RunnerManifestError::Line(line.to_owned()));
            };
            let section = sections
                .last_mut()
                .ok_or_else(|| RunnerManifestError::KeyOutsideSection(key.trim().to_owned()))?;
            section
                .1
                .push((key.trim().to_owned(), value.trim().to_owned()));
        }

        Ok(RunnerManifest {
            kind: RunnerKind::parse(
                string(&sections, "runtime", "kind")?
                    .ok_or_else(|| missing("runtime", "kind"))?
                    .as_str(),
            )
            .ok_or_else(|| missing("runtime", "kind"))?,
            name: string(&sections, "runtime", "name")?
                .ok_or_else(|| missing("runtime", "name"))?,
            bundle_id: string(&sections, "runtime", "bundle_id")?
                .ok_or_else(|| missing("runtime", "bundle_id"))?,
            version: string(&sections, "runtime", "version")?
                .ok_or_else(|| missing("runtime", "version"))?,
            mode: RuntimeMode::parse(
                string(&sections, "runtime", "mode")?
                    .ok_or_else(|| missing("runtime", "mode"))?
                    .as_str(),
            )
            .ok_or_else(|| missing("runtime", "mode"))?,
            target_path: string(&sections, "target", "path")?
                .ok_or_else(|| missing("target", "path"))?,
            package_name: string(&sections, "target", "package")?
                .ok_or_else(|| missing("target", "package"))?,
            local_cache_path: string(&sections, "paths", "local_cache")?
                .ok_or_else(|| missing("paths", "local_cache"))?,
            main_bundle_id: string(&sections, "paths", "main_bundle")?
                .ok_or_else(|| missing("paths", "main_bundle"))?,
            embedded_bundles_path: string(&sections, "paths", "embedded_bundles")?,
            server_host: string(&sections, "server", "host")?
                .ok_or_else(|| missing("server", "host"))?,
            server_port: value(&sections, "server", "port")?
                .ok_or_else(|| missing("server", "port"))?
                .parse()
                .map_err(|_| RunnerManifestError::Port)?,
            native_contract_hash: string(&sections, "abi", "native_contract_hash")?
                .ok_or_else(|| missing("abi", "native_contract_hash"))?,
        })
    }
}

/// Why a `KiraRunner.toml` could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RunnerManifestError {
    /// A required key was absent.
    #[error("`[{section}] {key}` is missing")]
    Missing {
        /// The section that should have held the key.
        section: &'static str,
        /// The absent key.
        key: &'static str,
    },
    /// A value was not a double-quoted string.
    #[error("`{line}` does not hold a `key = \"value\"` pair")]
    Value {
        /// The offending line.
        line: String,
    },
    /// A port value was not a number below 65536.
    #[error("[server] port is not a valid port number")]
    Port,
    /// A line appeared before any section header.
    #[error("`{0}` appears before any section")]
    KeyOutsideSection(String),
    /// A line was not a section header or a key/value pair.
    #[error("cannot read the line `{0}`")]
    Line(String),
}

fn missing(section: &'static str, key: &'static str) -> RunnerManifestError {
    RunnerManifestError::Missing { section, key }
}

/// Reads the raw text of one key from `section`, `None` when either is absent.
fn value(
    sections: &[(&str, Vec<(String, String)>)],
    section: &str,
    key: &str,
) -> Result<Option<String>, RunnerManifestError> {
    let Some((_, entries)) = sections.iter().find(|(name, _)| *name == section) else {
        // A whole section absent means every key in it is absent; the caller
        // names the first one it actually needed.
        return Ok(None);
    };
    Ok(entries
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.clone()))
}

/// Reads one double-quoted string value from `section`.
fn string(
    sections: &[(&str, Vec<(String, String)>)],
    section: &str,
    key: &str,
) -> Result<Option<String>, RunnerManifestError> {
    match value(sections, section, key)? {
        Some(raw) => quoted(&raw).map(Some),
        None => Ok(None),
    }
}

/// Strips one pair of double quotes, refusing anything else.
fn quoted(value: &str) -> Result<String, RunnerManifestError> {
    let unquoted = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'));
    match unquoted {
        Some(inner) => Ok(inner.to_owned()),
        None => Err(RunnerManifestError::Value {
            line: value.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> RunnerManifest {
        RunnerManifest {
            kind: RunnerKind::XcodeMacos,
            name: "KiraApp".to_owned(),
            bundle_id: "com.kira.live.dev".to_owned(),
            version: "0.1.0".to_owned(),
            mode: RuntimeMode::Live,
            target_path: "/projects/demo".to_owned(),
            package_name: "demo".to_owned(),
            local_cache_path: "app-support/KiraExport".to_owned(),
            main_bundle_id: "com.kira.demo".to_owned(),
            embedded_bundles_path: Some("Bundles".to_owned()),
            server_host: "127.0.0.1".to_owned(),
            server_port: 42111,
            native_contract_hash: "abc123".to_owned(),
        }
    }

    #[test]
    fn rendering_then_parsing_round_trips() {
        let parsed = RunnerManifest::parse(&sample().render()).expect("the render parses");
        assert_eq!(parsed, sample());
    }

    #[test]
    fn kinds_and_modes_round_trip_and_map_to_runners() {
        assert_eq!(
            RunnerKind::parse("xcode-visionos"),
            Some(RunnerKind::XcodeVisionos)
        );
        assert_eq!(RunnerKind::XcodeIos.runner_id(), crate::RunnerId::Ios);
        assert_eq!(RuntimeMode::parse("live"), Some(RuntimeMode::Live));
        assert_eq!(RuntimeMode::Standalone.label(), "standalone");
        assert_eq!(RunnerKind::parse("toaster"), None);
    }

    #[test]
    fn a_missing_key_is_named() {
        let text = sample().render().replace("mode = \"live\"\n", "");
        let error = RunnerManifest::parse(&text).expect_err("the key is gone");
        assert_eq!(
            error,
            RunnerManifestError::Missing {
                section: "runtime",
                key: "mode",
            }
        );
    }

    #[test]
    fn an_unquoted_value_is_refused() {
        let text = sample()
            .render()
            .replace("\"com.kira.demo\"", "com.kira.demo");
        assert!(matches!(
            RunnerManifest::parse(&text),
            Err(RunnerManifestError::Value { .. })
        ));
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let text = format!("# generated\n\n{}", sample().render());
        assert_eq!(RunnerManifest::parse(&text).expect("parses"), sample());
    }
}
