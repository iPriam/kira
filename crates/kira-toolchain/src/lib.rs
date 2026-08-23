//! Toolchain installation layout and discovery: KIRA_HOME, channels,
//! current-toolchain state, the packages bundled with an install, the pinned
//! LLVM metadata, and the managed LLVM bundle directory layout.
//!
//! Layer 0 of the Kira package graph.

pub mod bundled_discovery;
pub mod libffi_metadata;
pub mod llvm_code_generators;
pub mod llvm_discovery;
pub mod llvm_layout;
pub mod llvm_metadata;
pub mod paint;
pub mod pin;
pub mod process;
pub mod version;

pub use bundled_discovery::{
    BundledDiscoveryError, BundledPackage, BundledSource, discover_foundation,
    discover_foundation_from,
};
pub use libffi_metadata::{
    link_name_for,
    LibffiArchive, LibffiMetadata, LibffiPin, archive_for as libffi_archive_for,
    pinned as libffi_pinned, pinned_version as libffi_pinned_version, static_archive_name_for,
};
pub use llvm_code_generators::{CodeGeneratorError, WEB_CODE_GENERATOR};
pub use llvm_discovery::{
    DiscoverySource, LlvmDiscoveryError, LlvmInstallation, discover, is_llvm_home,
};
pub use llvm_metadata::{
    LlvmMetadata, MalformedMetadata, TargetBundle, bundle_for, pinned, pinned_version,
};
pub use paint::Paint;
pub use pin::{PIN_FILE_NAME, PinError, PinnedToolchain, find_pin, remove_pin, write_pin};
pub use version::RELEASE_VERSION;

use std::path::{Path, PathBuf};

/// The release channel a managed toolchain was installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Release,
    Dev,
}

impl Channel {
    /// Every channel, in the order a listing reports them.
    ///
    /// Stable rather than alphabetical: `release` is the default channel, so it
    /// reads first. Owned here so adding a channel cannot leave a caller
    /// walking a stale set.
    pub const ALL: [Self; 2] = [Self::Release, Self::Dev];

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "release" => Some(Self::Release),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }

    /// The directory name of the channel under `~/.kira/toolchains/`.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::Dev => "dev",
        }
    }
}

/// The `~/.kira/toolchains/current.toml` model: which installed toolchain the
/// `kira` launcher dispatches to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentToolchain {
    pub channel: Channel,
    pub version: String,
    /// The primary binary name the launcher executes (e.g. `kira`).
    pub primary: String,
}

impl CurrentToolchain {
    /// Serialize as `current.toml` text.
    pub fn to_toml(&self) -> String {
        format!(
            "channel = \"{}\"\nversion = \"{}\"\nprimary = \"{}\"\n",
            self.channel.dir_name(),
            self.version,
            self.primary,
        )
    }

    /// Parse `current.toml` contents.
    pub fn parse_toml(contents: &str) -> Result<Self, InvalidCurrentToolchain> {
        let raw: RawCurrentToolchain =
            toml::from_str(contents).map_err(|_| InvalidCurrentToolchain)?;
        Ok(Self {
            channel: Channel::parse(&raw.channel).ok_or(InvalidCurrentToolchain)?,
            version: raw.version,
            primary: raw.primary,
        })
    }
}

/// The on-disk shape of `current.toml`: three flat strings, nothing else.
///
/// Deserialized with the `toml` crate the crate already depends on rather than
/// by slicing quoted fields out of the text by hand.
#[derive(serde::Deserialize)]
struct RawCurrentToolchain {
    channel: String,
    version: String,
    primary: String,
}

/// Error returned when `current.toml` cannot be parsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidCurrentToolchain;

impl std::fmt::Display for InvalidCurrentToolchain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid current-toolchain state (current.toml)")
    }
}

impl std::error::Error for InvalidCurrentToolchain {}

/// Error returned when the user home directory cannot be determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HomeDirectoryUnavailable;

impl std::fmt::Display for HomeDirectoryUnavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "home directory unavailable (HOME/USERPROFILE unset)")
    }
}

impl std::error::Error for HomeDirectoryUnavailable {}

/// The user home directory (`HOME`, falling back to `USERPROFILE`).
pub fn home_dir() -> Result<PathBuf, HomeDirectoryUnavailable> {
    for var in ["HOME", "USERPROFILE"] {
        if let Some(home) = std::env::var_os(var)
            && !home.is_empty()
        {
            return Ok(PathBuf::from(home));
        }
    }
    Err(HomeDirectoryUnavailable)
}

/// The environment variable that relocates the Kira home directory wholesale.
pub const KIRA_HOME_VAR: &str = "KIRA_HOME";

/// `~/.kira` — the root of all user-level Kira state.
///
/// `KIRA_HOME` overrides it and never falls through, following the
/// `KIRA_FOUNDATION_HOME` / `KIRA_LLVM_HOME` precedent. That override is what
/// lets an installer or a launcher be driven against a throwaway root: a test
/// points a spawned child at a temp directory instead of the developer's real
/// `~/.kira`. Because every managed path below is derived from this function,
/// the override reaches the current-toolchain state and bundled-package
/// discovery rule 3 without either of them knowing about it.
pub fn kira_home() -> Result<PathBuf, HomeDirectoryUnavailable> {
    kira_home_from(std::env::var_os(KIRA_HOME_VAR))
}

/// [`kira_home`] with the override supplied explicitly, so it is testable
/// without mutating the process environment.
fn kira_home_from(
    explicit: Option<std::ffi::OsString>,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    if let Some(explicit) = explicit
        && !explicit.is_empty()
    {
        return Ok(PathBuf::from(explicit));
    }
    Ok(home_dir()?.join(".kira"))
}

/// `~/.kira/toolchains` — the root of all managed toolchains.
pub fn toolchains_root() -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(kira_home()?.join("toolchains"))
}

/// `~/.kira/toolchains/current.toml` — the current-toolchain state file.
pub fn current_toolchain_path() -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(toolchains_root()?.join("current.toml"))
}

/// `~/.kira/toolchains/<channel>/<version>` — an installed toolchain root.
pub fn managed_toolchain_root(
    channel: Channel,
    version: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(toolchains_root()?.join(channel.dir_name()).join(version))
}

/// `~/.kira/toolchains/<channel>/<version>/bin` — an installed toolchain's binaries.
pub fn managed_binary_dir(
    channel: Channel,
    version: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_toolchain_root(channel, version)?.join("bin"))
}

/// The language server binary a toolchain ships beside its primary.
///
/// One spelling shared by everything that speaks it: knvm stages and validates
/// the binary under this name, the release workflow packages it, and the
/// `kira` launcher dispatches to it when invoked under this name — so an
/// editor finds the selected toolchain's server on PATH, never a stale copy.
pub const LANGUAGE_SERVER_BINARY: &str = "kira-language-server";

/// The stack a Windows executable reserves for its main thread, in bytes.
///
/// A PE reserves 1 MiB unless it is told otherwise, which is an eighth of what
/// every other platform gives a main thread. Two things overflow it: a Kira
/// program with a deep, wide call graph, and the compiler's own frontend
/// recursing over a large package's syntax, types, and IR.
///
/// So both get this reserve — the executables the LLVM backend links, through
/// its link line, and the toolchain's own binaries, through the workspace's
/// `.cargo/config.toml`. Cargo cannot read a constant, so that file carries
/// this number as a literal and names this constant beside it.
pub const WINDOWS_STACK_RESERVE: u64 = 32 * 1024 * 1024;

/// The desktop runner client a toolchain ships beside its primary.
///
/// `kira live` starts this next to itself, so it has to be installed with the
/// compiler rather than left in a checkout's `target/debug`: a toolchain
/// without it can build a bundle and has nowhere to run it. One spelling
/// shared by the compiler that spawns it, knvm's staging, and the release
/// workflow that packages it.
pub const DESKTOP_RUNNER_BINARY: &str = "kira-desktop-runner";

/// The full path of an installed toolchain's primary binary.
pub fn managed_primary_binary_path(
    channel: Channel,
    version: &str,
    primary: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_binary_dir(channel, version)?.join(executable_name(primary)))
}

/// `~/.kira/toolchains/llvm` — the root of user-level managed LLVM bundles.
pub fn managed_llvm_root() -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(toolchains_root()?.join("llvm"))
}

/// `~/.kira/toolchains/llvm/<llvm-version>`.
pub fn managed_llvm_version_root(llvm_version: &str) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_llvm_root()?.join(llvm_version))
}

/// `~/.kira/toolchains/llvm/<llvm-version>/<host-key>` — an installed LLVM home.
pub fn managed_llvm_home(
    llvm_version: &str,
    host_key: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_llvm_version_root(llvm_version)?.join(host_key))
}

/// `~/.kira/toolchains/libffi` — the root of user-level managed libffi bundles.
pub fn managed_libffi_root() -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(toolchains_root()?.join("libffi"))
}

/// `~/.kira/toolchains/libffi/<libffi-version>`.
pub fn managed_libffi_version_root(
    libffi_version: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_libffi_root()?.join(libffi_version))
}

/// `~/.kira/toolchains/libffi/<libffi-version>/<target-key>` — an installed
/// libffi home.
///
/// Keyed by target rather than by host, unlike the LLVM bundle beside it: libffi
/// is linked into the artifact, so a cross build needs the archive for the
/// machine it emits for. One host can therefore hold several.
pub fn managed_libffi_home(
    libffi_version: &str,
    host_key: &str,
) -> Result<PathBuf, HomeDirectoryUnavailable> {
    Ok(managed_libffi_version_root(libffi_version)?.join(host_key))
}

/// Append `.exe` on Windows hosts; the base name everywhere else.
pub fn executable_name(base: &str) -> String {
    if cfg!(windows) {
        format!("{base}.exe")
    } else {
        base.to_string()
    }
}

/// The file name a static archive has on this host.
///
/// `<base>.lib` under MSVC, `lib<base>.a` everywhere else — the same split
/// [`executable_name`] makes for `.exe`, for the other artifact kind a
/// toolchain ships. The discriminator is the toolchain rather than the OS:
/// a GNU-toolchain Windows build still writes `lib<base>.a`.
pub fn static_archive_name(base: &str) -> String {
    if cfg!(target_env = "msvc") {
        format!("{base}.lib")
    } else {
        format!("lib{base}.a")
    }
}

/// `<libffi-home>/lib/<archive>` — the static archive a build links.
///
/// The archive is the whole of what an installed libffi home is for. Kira links
/// libffi statically, so this file is consumed at build time and nothing is
/// shipped beside the artifact that results: a user downloads Kira and has the
/// engine already.
#[must_use]
pub fn managed_libffi_archive(libffi_home: &Path, os: &str, env: &str) -> PathBuf {
    libffi_home
        .join("lib")
        .join(static_archive_name_for(os, env))
}

/// Whether `path` looks like an installed libffi home rather than some other
/// directory that happens to exist.
///
/// The check is the archive itself, because that is the only file anything
/// reads: a home with headers and no archive would pass a directory test and
/// then fail at the link, naming a missing symbol instead of a missing install.
#[must_use]
pub fn is_libffi_home(path: &Path, os: &str, env: &str) -> bool {
    managed_libffi_archive(path, os, env).is_file()
}

/// The key naming one target's libffi archive, or `None` where Kira publishes
/// none.
///
/// This is the spelling `libffi-metadata.toml` keys its target table by and the
/// directory name an installed archive sits in, so the build script that links
/// one and the installer that fetches one cannot disagree about which machine
/// they mean. A target outside the set links no engine, and every foreign
/// import fails where it is written rather than at a link that quietly
/// resolved nothing.
pub fn libffi_vendor_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("windows-x86_64"),
        ("macos", "aarch64") => Some("macos-aarch64"),
        ("macos", "x86_64") => Some("macos-x86_64"),
        ("linux", "aarch64") => Some("linux-aarch64"),
        ("linux", "x86_64") => Some("linux-x86_64"),
        _ => None,
    }
}

/// `<llvm-home>/bin/<tool>` — the path of a tool inside a managed LLVM home.
/// Existence checks live with the caller during the port.
pub fn managed_llvm_tool_path(llvm_home: &Path, tool_name: &str) -> PathBuf {
    llvm_home.join("bin").join(executable_name(tool_name))
}

/// `<llvm-home>/bin/clang`.
pub fn managed_llvm_clang_path(llvm_home: &Path) -> PathBuf {
    managed_llvm_tool_path(llvm_home, "clang")
}

/// `<llvm-home>/bin/llvm-ar`.
pub fn managed_llvm_ar_path(llvm_home: &Path) -> PathBuf {
    managed_llvm_tool_path(llvm_home, "llvm-ar")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_parses_current_toolchain_toml() {
        let current = CurrentToolchain {
            channel: Channel::Release,
            version: "0.1.0".to_string(),
            primary: "kira".to_string(),
        };
        let text = current.to_toml();
        let parsed = CurrentToolchain::parse_toml(&text).unwrap();
        assert_eq!(current, parsed);
    }

    #[test]
    fn rejects_current_toolchain_toml_that_is_not_the_schema() {
        assert!(CurrentToolchain::parse_toml("channel = \"release\"\n").is_err());
        assert!(
            CurrentToolchain::parse_toml(
                "channel = \"nightly\"\nversion = \"1.0.0\"\nprimary = \"kira\"\n"
            )
            .is_err()
        );
        assert!(CurrentToolchain::parse_toml("not toml at all {{{").is_err());
    }

    #[test]
    fn kira_home_honors_the_explicit_override() {
        let overridden = kira_home_from(Some(std::ffi::OsString::from("/tmp/knvm-fixture-home")))
            .expect("override needs no home directory");
        assert_eq!(overridden, PathBuf::from("/tmp/knvm-fixture-home"));

        // An empty override is treated as unset rather than as the root.
        let empty = kira_home_from(Some(std::ffi::OsString::new()));
        match (empty, home_dir()) {
            (Ok(path), Ok(home)) => assert_eq!(path, home.join(".kira")),
            (Err(_), Err(_)) => {}
            _ => panic!("empty override must behave exactly as an unset one"),
        }
    }

    #[test]
    fn builds_managed_toolchain_layout() {
        let Ok(root) = managed_toolchain_root(Channel::Dev, "0.1.0") else {
            return; // no home directory in this environment
        };
        let expected: PathBuf = [".kira", "toolchains", "dev", "0.1.0"].iter().collect();
        assert!(root.ends_with(&expected));

        let llvm_home = managed_llvm_home("21.1.2", "x86_64-linux-gnu").unwrap();
        let expected: PathBuf = [".kira", "toolchains", "llvm", "21.1.2", "x86_64-linux-gnu"]
            .iter()
            .collect();
        assert!(llvm_home.ends_with(&expected));

        let libffi_home = managed_libffi_home("3.5.2", "x86_64-linux-gnu").unwrap();
        let expected: PathBuf = [".kira", "toolchains", "libffi", "3.5.2", "x86_64-linux-gnu"]
            .iter()
            .collect();
        assert!(libffi_home.ends_with(&expected));
    }

    /// The workspace's cargo configuration cannot read a constant, so this is
    /// what keeps the number it passes to the linker equal to the one the
    /// backend reserves for the programs Kira produces.
    #[test]
    fn the_workspace_reserves_the_same_windows_stack_for_its_own_binaries() {
        let config = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("..")
            .join(".cargo")
            .join("config.toml");
        let Ok(text) = std::fs::read_to_string(&config) else {
            return; // built from a package rather than from the workspace
        };
        let expected = format!("/STACK:{WINDOWS_STACK_RESERVE}");
        assert!(
            text.contains(&expected),
            "{} does not reserve {expected}",
            config.display()
        );
    }
}
