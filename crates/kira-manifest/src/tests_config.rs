//! The `Tests { backends: [...], phase: ... }` manifest declaration.

use crate::platform_config::Backend;

/// Which portion of a `Test` / `FailTest` suite the runner exercises when a
/// package's manifest carries a `Tests { ... }` declaration.
///
/// * `Check` — compile/analyze every `Test` and `FailTest` without executing
///   `Test` bodies. `FailTest` cases still evaluate: they are compile-time
///   negative checks that assert a diagnostic outcome.
/// * `Run` — execute `Test` bodies (and evaluate `FailTest`s).
/// * `Both` — do the check pass and the run pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TestPhase {
    Check,
    #[default]
    Run,
    Both,
}

impl TestPhase {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "check" | "Check" => Some(Self::Check),
            "run" | "Run" => Some(Self::Run),
            "both" | "Both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::Run => "run",
            Self::Both => "both",
        }
    }
}

/// The `Tests { backends: [...], phase: ... }` manifest declaration honored by
/// `kira test`. `backends` is the matrix the runner iterates (each must end
/// 0-failed); `phase` selects check/run/both. A `None` `TestsConfig` on a
/// `ProjectManifest` means the manifest omitted the field and the runner keeps
/// its historical single-backend behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TestsConfig {
    pub backends: Vec<Backend>,
    pub phase: TestPhase,
}
