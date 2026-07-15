//! Platform/backend configuration enums, execution policy, and the resolved
//! profile/runner matrix.
//!
//! Split into `backends` (backend selection + execution policy), `web` (Web
//! surface model), and `resolved` (profile/runner matrix). The public surface
//! is re-exported flat from this module.

pub mod backends;
pub mod resolved;
pub mod web;

pub use backends::{
    Backend, BackendSelectionSource, ExecutionBackend, ExecutionPolicy, HybridSelectionMode,
    LibraryExecutionPolicy, WebExecutionPolicy,
};
pub use resolved::{
    ProfileConfig, ProfileSectionError, ResolvedConfig, RunnerConfig, default_resolved_config,
    validate_profile_section,
};
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
