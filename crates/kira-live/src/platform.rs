//! Live platform identity: maps manifest runner ids to concrete runners.
//!
//! Ported from kira-zig `kira_live/src/platform.zig`.

use crate::runner_kind::RunnerKind;

pub use kira_manifest::RunnerId;

/// The live platform id (same domain as the manifest runner id).
pub type LivePlatform = RunnerId;

/// Parse a user-facing runner id (including aliases like `ios-simulator`).
pub fn parse_runner_id(text: &str) -> Option<RunnerId> {
    RunnerId::parse(text)
}

/// The concrete runner behind a platform id.
pub fn runner_kind(id: RunnerId) -> RunnerKind {
    match id {
        RunnerId::Desktop => RunnerKind::DesktopDynamicHost,
        RunnerId::Macos => RunnerKind::XcodeMacos,
        RunnerId::Ios => RunnerKind::XcodeIos,
        RunnerId::Tvos => RunnerKind::XcodeTvos,
        RunnerId::Visionos => RunnerKind::XcodeVisionos,
        RunnerId::Windows => RunnerKind::WindowsVisualStudio,
        RunnerId::Android => RunnerKind::AndroidGradle,
        RunnerId::Web => RunnerKind::WebKiraWasm,
        RunnerId::Linux => RunnerKind::LinuxCmake,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_user_facing_aliases() {
        assert_eq!(Some(RunnerId::Desktop), parse_runner_id("desktop"));
        assert_eq!(Some(RunnerId::Ios), parse_runner_id("ios-simulator"));
        assert_eq!(RunnerKind::WebKiraWasm, runner_kind(RunnerId::Web));
    }
}
