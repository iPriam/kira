//! Target selectors and per-target library resolution.
//!
//! Ported from kira-zig `kira_native_lib_definition/src/target_resolution.zig`.

use crate::link_extras::LinkExtras;
use crate::native_library::{AutobindingSpec, BuildRecipe, HeaderSpec, LibraryAbi, LinkMode};

/// An `arch-os-abi` target triple, split (Zig `TargetSelector`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TargetSelector {
    /// Zig `architecture: []const u8`.
    pub architecture: String,
    /// Zig `operating_system: []const u8`.
    pub operating_system: String,
    /// Zig `abi: []const u8`.
    pub abi: String,
}

// TODO(port): `TargetSelector.parse(triple)` — split "arch-os-abi", erroring
// on missing components (Zig `error.InvalidManifest`).

/// Why a declared native library must be skipped for the active target
/// (Zig `Unavailable.Reason`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Zig `.missing_environment_variable` — a manifest path references an
    /// unset environment variable (e.g. `${VULKAN_SDK}`).
    MissingEnvironmentVariable,
}

/// Records why a declared native library could not be resolved and must be
/// skipped rather than aborting the whole preparation pass (Zig `Unavailable`).
#[derive(Debug, Clone, PartialEq)]
pub struct Unavailable {
    /// Zig `reason: Reason`.
    pub reason: UnavailableReason,
    /// Zig `detail` — reason-specific detail (the unset variable's name for
    /// `MissingEnvironmentVariable`).
    pub detail: String,
}

/// A build-time asset directory (or file) bundled into a self-contained
/// `wasm32-emscripten` package (Zig `AssetMount`). `host_path` is the absolute
/// on-disk source; `mount_path` is the MEMFS location the running app reads.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetMount {
    /// Zig `host_path: []const u8`.
    pub host_path: String,
    /// Zig `mount_path: []const u8`.
    pub mount_path: String,
}

/// A library resolved for one concrete target (Zig `ResolvedNativeLibrary`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedNativeLibrary {
    /// Zig `manifest_path: ?[]const u8`.
    pub manifest_path: Option<String>,
    /// Zig `name: []const u8`.
    pub name: String,
    /// Zig `link_mode: LinkMode`.
    pub link_mode: LinkMode,
    /// Zig `abi: LibraryAbi`.
    pub abi: LibraryAbi,
    /// Zig `artifact_path` — empty for pure-linkage targets that only
    /// contribute system frameworks/libraries.
    pub artifact_path: String,
    /// Zig `target: TargetSelector`.
    pub target: TargetSelector,
    /// Zig `headers: HeaderSpec`.
    pub headers: HeaderSpec,
    /// Zig `autobinding: ?AutobindingSpec`.
    pub autobinding: Option<AutobindingSpec>,
    /// Zig `build: BuildRecipe = .{}`.
    pub build: BuildRecipe,
    /// Zig `compiler_flags` — carried from the matching `TargetSpec`.
    pub compiler_flags: Vec<String>,
    /// Zig `link: LinkExtras`.
    pub link: LinkExtras,
    /// Zig `unavailable` — when set, every preparation step must skip this
    /// library and surface a warning instead of failing.
    pub unavailable: Option<Unavailable>,
}

// TODO(port): `resolve_library(spec, active_target) -> ResolvedNativeLibrary`
// — target matching, the pure-linkage (no artifact) rule, and the
// `UnsupportedTarget` error (Zig `resolveLibrary`).
