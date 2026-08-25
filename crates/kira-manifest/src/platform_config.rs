//! Platform/backend configuration enums, execution policy, and the resolved
//! profile/runner matrix.
//!
//! Split into `backends` (backend selection + execution policy), `web` (Web
//! surface model), and `resolved` (profile/runner matrix). The public surface
//! is re-exported flat from this module.

pub mod backends;
pub mod resolved;
pub mod runner_manifest;
pub mod web;

pub use backends::{
    Backend, BackendSelectionSource, ExecutionBackend, ExecutionPolicy, HybridSelectionMode,
    LibraryExecutionPolicy, WebExecutionPolicy,
};
pub use resolved::{
    ProfileConfig, ProfileSectionError, ResolvedConfig, RunnerConfig, default_resolved_config,
    validate_profile_section,
};
pub use runner_manifest::{RunnerKind, RunnerManifest, RunnerManifestError, RuntimeMode};
pub use web::{
    WebGraphicsBridge, WebGraphicsCapability, WebRenderingModel, WebSurface,
    WebSurfaceRequirements, web_surface_requirements,
};

/// A platform runner target the CLI can build/live against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerId {
    Desktop,
    Macos,
    Ios,
    Tvos,
    Visionos,
    Windows,
    Android,
    Web,
    Linux,
}

impl RunnerId {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "desktop" => Some(Self::Desktop),
            "macos" => Some(Self::Macos),
            "ios" | "ios-simulator" | "ios-device" => Some(Self::Ios),
            "tvos" => Some(Self::Tvos),
            "visionos" => Some(Self::Visionos),
            "windows" => Some(Self::Windows),
            "android" => Some(Self::Android),
            "web" => Some(Self::Web),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Desktop => "desktop",
            Self::Macos => "macos",
            Self::Ios => "ios",
            Self::Tvos => "tvos",
            Self::Visionos => "visionos",
            Self::Windows => "windows",
            Self::Android => "android",
            Self::Web => "web",
            Self::Linux => "linux",
        }
    }

    /// This runner's slot in a resolved matrix, and the count of all runners.
    ///
    /// A resolved matrix stores one row per runner *at that runner's own
    /// index*, so a lookup is an index rather than a search that could fail to
    /// find its row.
    pub const COUNT: usize = 9;

    /// This runner's slot in a resolved matrix; always below [`RunnerId::COUNT`].
    ///
    /// **This ordering is a wire format.** `kira-live` writes a runner into a
    /// `KLB1` bundle as this index, so renumbering here changes what every
    /// bundle already on disk decodes to — it is append-only, like any other
    /// serialized tag. A new runner goes on the end. `kira-live`'s
    /// `runner_wire_bytes_are_pinned` test spells the bytes out and fails if
    /// this moves.
    pub fn index(self) -> usize {
        match self {
            Self::Desktop => 0,
            Self::Macos => 1,
            Self::Ios => 2,
            Self::Tvos => 3,
            Self::Visionos => 4,
            Self::Windows => 5,
            Self::Android => 6,
            Self::Web => 7,
            Self::Linux => 8,
        }
    }

    /// Every runner, in index order.
    pub fn all() -> [RunnerId; RunnerId::COUNT] {
        [
            Self::Desktop,
            Self::Macos,
            Self::Ios,
            Self::Tvos,
            Self::Visionos,
            Self::Windows,
            Self::Android,
            Self::Web,
            Self::Linux,
        ]
    }
}

/// The external build system a runner is driven through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildSystem {
    Kira,
    Xcode,
    VisualStudio,
    AndroidStudio,
    KiraWasm,
    Cmake,
}

impl BuildSystem {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "kira" => Some(Self::Kira),
            "xcode" => Some(Self::Xcode),
            "visual-studio" | "visual_studio" => Some(Self::VisualStudio),
            "android-studio" | "android_studio" => Some(Self::AndroidStudio),
            "kira-wasm" | "kira_wasm" => Some(Self::KiraWasm),
            "cmake" => Some(Self::Cmake),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Kira => "kira",
            Self::Xcode => "xcode",
            Self::VisualStudio => "visual-studio",
            Self::AndroidStudio => "android-studio",
            Self::KiraWasm => "kira-wasm",
            Self::Cmake => "cmake",
        }
    }
}

/// The build profile selected for a runner (`profile` is a reserved name).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildProfile {
    Debug,
    Profiler,
    Release,
}

impl BuildProfile {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(Self::Debug),
            "profiler" => Some(Self::Profiler),
            "release" => Some(Self::Release),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Profiler => "profiler",
            Self::Release => "release",
        }
    }

    /// How many build profiles there are.
    ///
    /// A resolved matrix stores one row per profile *at that profile's own
    /// index*, so a lookup is an index rather than a search that could fail to
    /// find its row.
    pub const COUNT: usize = 3;

    /// This profile's slot in a resolved matrix; always below
    /// [`BuildProfile::COUNT`].
    ///
    /// **This ordering is a wire format**, for the same reason as
    /// [`RunnerId::index`]: a `KLB1` bundle records its profile as this index.
    /// Append-only; `kira-live`'s `profile_wire_bytes_are_pinned` guards it.
    pub fn index(self) -> usize {
        match self {
            Self::Debug => 0,
            Self::Profiler => 1,
            Self::Release => 2,
        }
    }

    /// Every build profile, in index order.
    pub fn all() -> [BuildProfile; BuildProfile::COUNT] {
        [Self::Debug, Self::Profiler, Self::Release]
    }
}

/// The export family selected by `kira export`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFamily {
    Apple,
    Macos,
    Ios,
    Tvos,
    Visionos,
    Windows,
    Android,
    Web,
    Linux,
}

impl ExportFamily {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "apple" => Some(Self::Apple),
            "macos" => Some(Self::Macos),
            "ios" => Some(Self::Ios),
            "tvos" => Some(Self::Tvos),
            "visionos" => Some(Self::Visionos),
            "windows" => Some(Self::Windows),
            "android" => Some(Self::Android),
            "web" => Some(Self::Web),
            "linux" => Some(Self::Linux),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Apple => "apple",
            Self::Macos => "macos",
            Self::Ios => "ios",
            Self::Tvos => "tvos",
            Self::Visionos => "visionos",
            Self::Windows => "windows",
            Self::Android => "android",
            Self::Web => "web",
            Self::Linux => "linux",
        }
    }
}

/// Apple platforms addressed by the Xcode-driven runners.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplePlatform {
    Macos,
    Ios,
    Tvos,
    Visionos,
}

impl ApplePlatform {
    pub fn label(self) -> &'static str {
        match self {
            Self::Macos => "macOS",
            Self::Ios => "iOS",
            Self::Tvos => "tvOS",
            Self::Visionos => "visionOS",
        }
    }
}

/// The live-reload protocol mode (currently only full-bundle transfer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiveProtocolMode {
    FullBundle,
}
