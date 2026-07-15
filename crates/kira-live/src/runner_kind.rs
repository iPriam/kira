//! The concrete runner implementations behind each live platform.
//!
//! Ported from kira-zig `kira_live/src/runner_kind.zig`.

/// A concrete platform runner (platform + build system pairing).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunnerKind {
    DesktopDynamicHost,
    XcodeMacos,
    XcodeIos,
    XcodeTvos,
    XcodeVisionos,
    WindowsVisualStudio,
    AndroidGradle,
    WebKiraWasm,
    LinuxCmake,
}

impl RunnerKind {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "desktop" | "desktop-dynamic-host" => Some(Self::DesktopDynamicHost),
            "macos" | "xcode-macos" => Some(Self::XcodeMacos),
            "ios" | "xcode-ios" => Some(Self::XcodeIos),
            "tvos" | "xcode-tvos" => Some(Self::XcodeTvos),
            "visionos" | "xcode-visionos" => Some(Self::XcodeVisionos),
            "windows" | "visual-studio-windows" => Some(Self::WindowsVisualStudio),
            "android" | "android-gradle" => Some(Self::AndroidGradle),
            "web" | "kira-wasm" => Some(Self::WebKiraWasm),
            "linux" | "linux-cmake" => Some(Self::LinuxCmake),
            _ => None,
        }
    }

    /// The short user-facing CLI name.
    pub fn cli_name(self) -> &'static str {
        match self {
            Self::DesktopDynamicHost => "desktop",
            Self::XcodeMacos => "macos",
            Self::XcodeIos => "ios",
            Self::XcodeTvos => "tvos",
            Self::XcodeVisionos => "visionos",
            Self::WindowsVisualStudio => "windows",
            Self::AndroidGradle => "android",
            Self::WebKiraWasm => "web",
            Self::LinuxCmake => "linux",
        }
    }

    /// The canonical name written into runner manifests.
    pub fn manifest_name(self) -> &'static str {
        match self {
            Self::DesktopDynamicHost => "desktop-dynamic-host",
            Self::XcodeMacos => "xcode-macos",
            Self::XcodeIos => "xcode-ios",
            Self::XcodeTvos => "xcode-tvos",
            Self::XcodeVisionos => "xcode-visionos",
            Self::WindowsVisualStudio => "windows-visual-studio",
            Self::AndroidGradle => "android-gradle",
            Self::WebKiraWasm => "web-kira-wasm",
            Self::LinuxCmake => "linux-cmake",
        }
    }

    /// Deterministic per-runner directory name (equals the manifest name).
    pub fn deterministic_directory_name(self) -> &'static str {
        self.manifest_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_prints_canonical_names() {
        assert_eq!(
            Some(RunnerKind::DesktopDynamicHost),
            RunnerKind::parse("desktop")
        );
        assert_eq!(Some(RunnerKind::XcodeIos), RunnerKind::parse("xcode-ios"));
        assert_eq!("web-kira-wasm", RunnerKind::WebKiraWasm.manifest_name());
        assert_eq!("web", RunnerKind::WebKiraWasm.cli_name());
    }
}
