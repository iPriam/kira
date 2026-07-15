//! The resolved profile/runner matrix and its defaults.

use super::backends::Backend;
use super::web::WebSurface;
use super::{BuildProfile, BuildSystem, RunnerId};

/// A resolved build profile row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileConfig {
    pub id: BuildProfile,
    pub backend: Backend,
    pub optimization: String,
    pub debug_symbols: bool,
    pub profiling: bool,
    pub strip: bool,
    pub lto: bool,
}

/// A resolved runner row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerConfig {
    pub id: RunnerId,
    pub build_system: BuildSystem,
    pub default_profile: BuildProfile,
    pub default_surface: Option<WebSurface>,
}

/// The full resolved profile/runner matrix of a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedConfig {
    pub profiles: [ProfileConfig; 3],
    pub runners: [RunnerConfig; 9],
}

impl ResolvedConfig {
    pub fn profile(&self, id: BuildProfile) -> &ProfileConfig {
        self.profiles
            .iter()
            .find(|item| item.id == id)
            .expect("resolved config always carries all three profiles")
    }

    pub fn runner(&self, id: RunnerId) -> &RunnerConfig {
        self.runners
            .iter()
            .find(|item| item.id == id)
            .expect("resolved config always carries all nine runners")
    }
}

/// The default profile/runner matrix synthesized when a manifest does not
/// override platform configuration.
pub fn default_resolved_config() -> ResolvedConfig {
    let profile =
        |id, backend, optimization: &str, debug_symbols, profiling, strip, lto| ProfileConfig {
            id,
            backend,
            optimization: optimization.to_string(),
            debug_symbols,
            profiling,
            strip,
            lto,
        };
    let runner = |id, build_system, default_surface| RunnerConfig {
        id,
        build_system,
        default_profile: BuildProfile::Debug,
        default_surface,
    };
    ResolvedConfig {
        profiles: [
            profile(
                BuildProfile::Debug,
                Backend::Vm,
                "none",
                true,
                false,
                false,
                false,
            ),
            profile(
                BuildProfile::Profiler,
                Backend::Llvm,
                "speed-lite",
                true,
                true,
                false,
                false,
            ),
            profile(
                BuildProfile::Release,
                Backend::Llvm,
                "speed",
                false,
                false,
                true,
                true,
            ),
        ],
        runners: [
            runner(RunnerId::Desktop, BuildSystem::Kira, None),
            runner(RunnerId::Macos, BuildSystem::Xcode, None),
            runner(RunnerId::Ios, BuildSystem::Xcode, None),
            runner(RunnerId::Tvos, BuildSystem::Xcode, None),
            runner(RunnerId::Visionos, BuildSystem::Xcode, None),
            runner(RunnerId::Windows, BuildSystem::VisualStudio, None),
            runner(RunnerId::Android, BuildSystem::AndroidStudio, None),
            runner(RunnerId::Web, BuildSystem::KiraWasm, Some(WebSurface::Dom)),
            runner(RunnerId::Linux, BuildSystem::Cmake, None),
        ],
    }
}

/// Validate a `profiles.<name>` manifest section name. `profiles.profile` is
/// reserved; only the known profile names are accepted.
pub fn validate_profile_section(section: &str) -> Result<(), ProfileSectionError> {
    if section == "profiles.profile" {
        return Err(ProfileSectionError::ReservedProfileName);
    }
    let Some(name) = section.strip_prefix("profiles.") else {
        return Ok(());
    };
    if BuildProfile::parse(name).is_none() {
        return Err(ProfileSectionError::UnknownProfile);
    }
    Ok(())
}

/// Errors from [`validate_profile_section`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSectionError {
    ReservedProfileName,
    UnknownProfile,
}

impl std::fmt::Display for ProfileSectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReservedProfileName => write!(f, "`profiles.profile` is a reserved section"),
            Self::UnknownProfile => write!(f, "unknown profile section"),
        }
    }
}

impl std::error::Error for ProfileSectionError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_platform_config_synthesizes_profiles_and_runners() {
        let config = default_resolved_config();
        assert_eq!(Backend::Vm, config.profile(BuildProfile::Debug).backend);
        assert_eq!(
            Backend::Llvm,
            config.profile(BuildProfile::Profiler).backend
        );
        assert!(config.profile(BuildProfile::Profiler).profiling);
        assert!(config.profile(BuildProfile::Release).lto);
        assert_eq!(
            BuildSystem::KiraWasm,
            config.runner(RunnerId::Web).build_system
        );
        assert_eq!(
            Some(WebSurface::Dom),
            config.runner(RunnerId::Web).default_surface
        );
        assert_eq!(
            BuildSystem::AndroidStudio,
            config.runner(RunnerId::Android).build_system
        );
    }

    #[test]
    fn profile_is_reserved_profiler_is_supported() {
        assert!(validate_profile_section("profiles.profiler").is_ok());
        assert_eq!(
            Err(ProfileSectionError::ReservedProfileName),
            validate_profile_section("profiles.profile")
        );
    }
}
