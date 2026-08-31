//! What the frontend is analyzing a program *as*: an application or a library.
//!
//! # Why this lives here and not in `kira-manifest`
//!
//! The manifest spells the same choice as [`kira_manifest::PackageKind`], but
//! `kira-manifest` is layer 5 and this crate is layer 2, so naming it here
//! would be an upward dependency. The two spellings are deliberate: the
//! manifest's is about a package on disk, this one is about one analysis run,
//! and the crate that has both — the CLI — maps between them. Nothing below
//! layer 5 gains a manifest.
//!
//! # Why analysis needs it at all
//!
//! An entrypoint is required by [`crate::analyze`], above the backend split, so a
//! library's absence of an entrypoint cannot be excused inside any one backend.
//! Making the requirement conditional is therefore a frontend input, not a
//! backend flag.

/// Whether a program is analyzed as a runnable application or as a library.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum BuildKind {
    /// A runnable program: it must declare exactly one lifecycle entry (`KSEM011`).
    ///
    /// The default, because a bare `.kira` file handed to the compiler with no
    /// manifest is a program someone means to run — and because defaulting the
    /// other way would silence the missing-entrypoint error for every
    /// application.
    #[default]
    Application,
    /// A library: it has no entrypoint, and declaring one is an error
    /// (`KSEM255`).
    Library,
    /// A test run: an entrypoint is neither required nor refused.
    ///
    /// A suite is entered through the runner a collector generated rather than
    /// through an entrypoint, so demanding one would refuse a package whose only
    /// purpose is tests. Refusing one would be worse: a package that is both an
    /// application and a suite is an ordinary thing to write, and `kira test`
    /// must not force it to choose. When an entrypoint is present it is recorded
    /// exactly as an application's is, so the two entrypoints coexist and the
    /// caller picks which one it enters.
    Test,
}

impl BuildKind {
    /// The word this kind is called in diagnostics and manifests.
    pub fn label(self) -> &'static str {
        match self {
            Self::Application => "application",
            Self::Library => "library",
            Self::Test => "test run",
        }
    }

    /// Whether a program of this kind must declare an entrypoint.
    pub fn requires_entrypoint(self) -> bool {
        matches!(self, Self::Application)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_application_requires_an_entrypoint() {
        assert!(BuildKind::Application.requires_entrypoint());
        assert!(!BuildKind::Library.requires_entrypoint());
    }

    #[test]
    fn the_default_is_the_runnable_kind() {
        // A file with no manifest is analyzed as a program, so a missing
        // `@Main` is still reported in the editor and on `kira check`.
        assert_eq!(BuildKind::default(), BuildKind::Application);
    }
}
