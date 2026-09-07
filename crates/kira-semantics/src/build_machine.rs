//! Which machine the program being analyzed is being built *for*.
//!
//! # Why this lives here and not in `kira-native-lib-definition`
//!
//! The two components below are exactly a target triple's `os` and `arch`, which
//! [`kira_native_lib_definition::TargetTriple`] already spells — but that crate
//! is layer 3 and this one is layer 2, so naming it here would be an upward
//! dependency. This is the same arrangement [`crate::BuildKind`] has with
//! `kira_manifest::PackageKind`: the crate that holds both, the build layer,
//! maps one to the other, and nothing below layer 3 gains a triple.
//!
//! The `abi` component is deliberately absent. No frontend rule turns on it —
//! `msvc` versus `gnu` changes how a C header binds, which happens in the build
//! layer, not in the analyzer.
//!
//! # Why analysis needs it at all
//!
//! Because a declaration can be *unavailable on a machine*, and the honest place
//! to say so is where the declaration is read. `@FFI.Syscall` is the case that
//! forced it: a Linux system call has no number on macOS that Kira would stand
//! behind and no lowering on a 32-bit machine, so a program naming one has to be
//! refused by name at compile time rather than emitted with a number that means
//! something else on the machine it runs on.
//!
//! Before this existed the frontend was handed the *host's* operating system
//! under the name `Build.platform`, whose own documentation said it was the one
//! the build targets. On a `kira build --target aarch64-linux-gnu` run from
//! Windows those are different answers, so a `comptime macro` selecting
//! platform-specific code selected the compiling machine's.

/// The machine a build is aimed at, as much of it as a frontend rule turns on.
///
/// Both components are spelled the way a target triple spells them —
/// `linux`/`macos`/`windows`, `x86_64`/`aarch64` — because every rule that reads
/// one is comparing it against a table keyed that way, and a second spelling
/// would be a second chance to compare `arm64` against `aarch64` and conclude
/// the architecture is unsupported.
#[derive(Debug, Clone, PartialEq, Eq, Hash, salsa::SalsaValue)]
pub struct BuildMachine {
    platform: String,
    architecture: String,
}

impl BuildMachine {
    /// Names the machine a build targets by its OS and architecture.
    pub fn new(platform: impl Into<String>, architecture: impl Into<String>) -> Self {
        Self {
            platform: platform.into(),
            architecture: architecture.into(),
        }
    }

    /// The machine running the compiler.
    ///
    /// The answer for every caller that names no target: an editor's analysis,
    /// the compiler-as-a-service check, and a plain `kira build` are all about
    /// this machine.
    pub fn host() -> Self {
        Self::new(host_platform(), host_architecture())
    }

    /// The operating system this build targets, as `Build.platform` spells it.
    pub fn platform(&self) -> &str {
        &self.platform
    }

    /// The processor architecture this build targets.
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

impl Default for BuildMachine {
    fn default() -> Self {
        Self::host()
    }
}

/// The operating system this compiler is running on, as `Build.platform`
/// spells it.
///
/// The default for a caller that names no target: building for the machine you
/// are on is what every plain `kira build` does.
#[must_use]
pub fn host_platform() -> String {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else if cfg!(target_os = "ios") {
        "ios"
    } else {
        kira_macros::UNKNOWN_PLATFORM
    }
    .to_owned()
}

/// The processor architecture this compiler is running on, spelled the way a
/// target triple spells it.
///
/// `std::env::consts::ARCH` already uses that spelling — `x86_64`, `aarch64`,
/// `wasm32` — so it is taken verbatim rather than mapped, which is what keeps
/// this from disagreeing with a written triple about the same processor.
#[must_use]
pub fn host_architecture() -> String {
    std::env::consts::ARCH.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host answers about the machine the test is running on, in the
    /// spelling a triple would use — so a table keyed by triple component finds
    /// it.
    #[test]
    fn the_host_machine_is_spelled_the_way_a_triple_spells_it() {
        let host = BuildMachine::host();
        assert_eq!(host, BuildMachine::default());
        assert_eq!(host.architecture(), std::env::consts::ARCH);
        assert!(!host.platform().is_empty());
        if cfg!(target_os = "linux") {
            assert_eq!(host.platform(), "linux");
        }
    }

    /// A cross target is not the host, which is the whole reason this value
    /// exists: the two used to be the same answer under one name.
    #[test]
    fn a_named_machine_keeps_the_components_it_was_given() {
        // An architecture the host is not, whichever the host is.
        let other = if std::env::consts::ARCH == "aarch64" {
            "x86_64"
        } else {
            "aarch64"
        };
        let machine = BuildMachine::new("linux", other);
        assert_eq!(machine.platform(), "linux");
        assert_eq!(machine.architecture(), other);
        assert_ne!(machine, BuildMachine::host());
    }
}
