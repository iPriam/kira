//! Live bundle/runner manifest models.
//!
//! Ported from kira-zig `kira_live/src/model.zig`. The TOML write/parse
//! methods land with the port; these are the model types.

use crate::runner_kind::RunnerKind;

/// One package bundle inside a live session's bundle graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleSpec {
    pub id: String,
    pub package_name: String,
    pub package_root: String,
    pub version: String,
    pub kind: String,
    pub module_root: String,
    pub manifest_rel_path: String,
    pub bytecode_rel_path: String,
    pub hybrid_rel_path: String,
    pub executable: bool,
    pub validation_root: String,
}

/// The full bundle graph shipped to a runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleGraph {
    pub target_path: String,
    pub target_package: String,
    pub validation_app_path: String,
    pub main_bundle_id: String,
    pub bundles: Vec<BundleSpec>,
}

/// The manifest written inside a single bundle directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleManifest {
    pub id: String,
    pub package_name: String,
    pub version: String,
    pub kind: String,
    pub module_root: String,
    pub bytecode_rel_path: String,
    pub hybrid_rel_path: String,
    pub executable: bool,
}

/// Whether a runner executes as a live session or standalone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RuntimeMode {
    #[default]
    Live,
    Standalone,
}

impl RuntimeMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "live" => Some(Self::Live),
            "standalone" => Some(Self::Standalone),
            _ => None,
        }
    }

    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Standalone => "standalone",
        }
    }
}

/// The manifest a platform runner boots from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerManifest {
    pub kind: RunnerKind,
    pub name: String,
    pub bundle_id: String,
    pub version: String,
    pub target_path: String,
    pub package_name: String,
    pub validation_app_path: String,
    pub bundles_path: String,
    pub local_cache_path: String,
    pub main_bundle_id: String,
    pub server_host: String,
    pub server_port: u16,
    pub native_contract_hash: String,
    pub runtime_mode: RuntimeMode,
    pub embedded_bundles_path: Option<String>,
}
