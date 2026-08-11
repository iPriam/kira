//! Build requests, plans, targets, and artifact descriptors shared by build producers.
//!
//! Layer 5 of the Kira package graph. The CLI and library builders still own
//! their filesystem-specific layouts, but they now have one small vocabulary
//! for the request they are fulfilling and the artifacts they promise.

use std::path::{Path, PathBuf};

use kira_backend_api::{ArtifactKind, BackendMode};

/// The optimization/debug profile a build producer should use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BuildProfile {
    /// Fast iteration with debug assertions and normal development optimization.
    #[default]
    Dev,
    /// Debug symbols plus the caller's requested optimization level.
    Debug,
    /// Aggressive optimization for a shipped artifact.
    Release,
}

impl BuildProfile {
    /// The stable command-line spelling.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    /// Whether the native backend should select its aggressive optimization path.
    pub const fn optimized(self) -> bool {
        matches!(self, Self::Release)
    }
}

/// A backend-neutral build request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildRequest {
    /// The source file that seeds compilation.
    pub entry: PathBuf,
    /// The directory in which the producer places its artifacts.
    pub output_dir: PathBuf,
    /// The code-generation engine.
    pub backend: BackendMode,
    /// The target triple selected by the caller.
    pub target: String,
    /// The requested optimization/debug profile.
    pub profile: BuildProfile,
    /// Whether textual LLVM IR is part of the requested output.
    pub emit_llvm_ir: bool,
}

impl BuildRequest {
    /// Creates a request with the standard development profile.
    #[must_use]
    pub fn new(
        entry: impl Into<PathBuf>,
        output_dir: impl Into<PathBuf>,
        backend: BackendMode,
        target: impl Into<String>,
    ) -> Self {
        Self {
            entry: entry.into(),
            output_dir: output_dir.into(),
            backend,
            target: target.into(),
            profile: BuildProfile::default(),
            emit_llvm_ir: false,
        }
    }

    /// Changes the profile without changing the request's identity otherwise.
    #[must_use]
    pub const fn with_profile(mut self, profile: BuildProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Requests a textual LLVM IR sidecar.
    #[must_use]
    pub const fn with_llvm_ir(mut self, emit: bool) -> Self {
        self.emit_llvm_ir = emit;
        self
    }
}

/// Why a build plan could not describe the requested entry.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildPlanError {
    /// The entry path has no usable file stem for a deterministic artifact name.
    #[error("build entry `{path}` has no file stem")]
    MissingEntryStem {
        /// The path that could not be named.
        path: PathBuf,
    },
}

/// One artifact a build producer is expected to emit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactDescriptor {
    /// The backend-neutral kind of artifact.
    pub kind: ArtifactKind,
    /// Where the producer writes it.
    pub path: PathBuf,
}

/// The deterministic output contract for one build request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildPlan {
    /// The request this plan answers.
    pub request: BuildRequest,
    /// Artifacts in the order consumers should inspect them.
    pub artifacts: Vec<ArtifactDescriptor>,
}

impl BuildPlan {
    /// Derives the standard artifact names without touching the filesystem.
    pub fn for_request(request: BuildRequest) -> Result<Self, BuildPlanError> {
        let stem = request
            .entry
            .file_stem()
            .filter(|stem| !stem.is_empty())
            .map(|stem| stem.to_string_lossy().into_owned())
            .ok_or_else(|| BuildPlanError::MissingEntryStem {
                path: request.entry.clone(),
            })?;
        let mut artifacts = match request.backend {
            BackendMode::VmBytecode => vec![descriptor(
                ArtifactKind::Bytecode,
                request.output_dir.join(format!("{stem}.kbc")),
            )],
            BackendMode::LlvmNative => vec![
                descriptor(
                    ArtifactKind::NativeObject,
                    request.output_dir.join(format!("{stem}.o")),
                ),
                descriptor(
                    ArtifactKind::Executable,
                    request.output_dir.join(executable_name(&stem)),
                ),
            ],
            BackendMode::Hybrid => vec![
                descriptor(
                    ArtifactKind::Bytecode,
                    request.output_dir.join(format!("{stem}.kbc")),
                ),
                descriptor(
                    ArtifactKind::HybridBundle,
                    request.output_dir.join(format!("{stem}.khm")),
                ),
            ],
        };
        if request.emit_llvm_ir
            && matches!(
                request.backend,
                BackendMode::LlvmNative | BackendMode::Hybrid
            )
        {
            artifacts.push(descriptor(
                ArtifactKind::LlvmIr,
                request.output_dir.join(format!("{stem}.ll")),
            ));
        }
        Ok(Self { request, artifacts })
    }

    /// Finds the first artifact of `kind`, if this backend emits one.
    #[must_use]
    pub fn artifact(&self, kind: ArtifactKind) -> Option<&ArtifactDescriptor> {
        self.artifacts.iter().find(|artifact| artifact.kind == kind)
    }
}

fn descriptor(kind: ArtifactKind, path: PathBuf) -> ArtifactDescriptor {
    ArtifactDescriptor { kind, path }
}

fn executable_name(stem: &str) -> String {
    if cfg!(target_os = "windows") {
        format!("{stem}.exe")
    } else {
        stem.to_owned()
    }
}

/// Returns the final component used by a plan, without exposing platform suffix rules.
#[must_use]
pub fn artifact_stem(entry: &Path) -> Option<String> {
    entry
        .file_stem()
        .filter(|stem| !stem.is_empty())
        .map(|stem| stem.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_vm_request_has_one_bytecode_artifact() {
        let request = BuildRequest::new(
            "app/main.kira",
            ".kira-build",
            BackendMode::VmBytecode,
            "x86_64-linux-gnu",
        );
        let plan = BuildPlan::for_request(request).expect("plan");
        assert_eq!(plan.artifacts.len(), 1);
        assert_eq!(plan.artifacts[0].kind, ArtifactKind::Bytecode);
        assert_eq!(
            plan.artifacts[0].path,
            PathBuf::from(".kira-build/main.kbc")
        );
    }

    #[test]
    fn hybrid_ir_is_an_explicit_sidecar() {
        let request =
            BuildRequest::new("main.kira", "out", BackendMode::Hybrid, "host").with_llvm_ir(true);
        let plan = BuildPlan::for_request(request).expect("plan");
        assert_eq!(
            plan.artifacts
                .iter()
                .map(|artifact| artifact.kind)
                .collect::<Vec<_>>(),
            vec![
                ArtifactKind::Bytecode,
                ArtifactKind::HybridBundle,
                ArtifactKind::LlvmIr
            ]
        );
        assert!(plan.artifact(ArtifactKind::HybridBundle).is_some());
    }

    #[test]
    fn profile_labels_and_optimization_are_stable() {
        assert_eq!(BuildProfile::Dev.label(), "dev");
        assert!(!BuildProfile::Debug.optimized());
        assert!(BuildProfile::Release.optimized());
    }
}
