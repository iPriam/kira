//! Toolchain installation layout and discovery: KIRA_HOME, channels,
//! current-toolchain state, the packages bundled with an install, the pinned
//! LLVM metadata, and the managed LLVM bundle directory layout.
//!
//! Layer 0 of the Kira package graph.

pub mod bundled_discovery;
pub mod llvm_discovery;
pub mod llvm_layout;
pub mod llvm_metadata;

pub use bundled_discovery::{
    BundledDiscoveryError, BundledPackage, BundledSource, discover_foundation,
    discover_foundation_from,
};
pub use llvm_discovery::{DiscoverySource, LlvmDiscoveryError, LlvmInstallation, discover};
pub use llvm_metadata::{LlvmMetadata, MalformedMetadata, pinned, pinned_version};

use std::path::{Path, PathBuf};

/// The release channel a managed toolchain was installed from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Release,
    Dev,
}

impl Channel {
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
    /// The primary binary name the launcher executes (e.g. `kirac`).
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
        let channel_value = parse_quoted_field(contents, "channel")?;
        Ok(Self {
            channel: Channel::parse(&channel_value).ok_or(InvalidCurrentToolchain)?,
            version: parse_quoted_field(contents, "version")?,
            primary: parse_quoted_field(contents, "primary")?,
        })
    }
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

fn parse_quoted_field(contents: &str, field_name: &str) -> Result<String, InvalidCurrentToolchain> {
    let field_index = contents.find(field_name).ok_or(InvalidCurrentToolchain)?;
    let after_field = &contents[field_index + field_name.len()..];
    let equals_index = after_field.find('=').ok_or(InvalidCurrentToolchain)?;
    let after_equals = after_field[equals_index + 1..].trim_start_matches([' ', '\t', '\r', '\n']);
    let rest = after_equals
        .strip_prefix('"')
        .ok_or(InvalidCurrentToolchain)?;
    let closing_quote = rest.find('"').ok_or(InvalidCurrentToolchain)?;
    Ok(rest[..closing_quote].to_string())
}

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

/// `~/.kira` — the root of all user-level Kira state.
pub fn kira_home() -> Result<PathBuf, HomeDirectoryUnavailable> {
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

/// `~/.kira/toolchains/libffi/<libffi-version>/<host-key>` — an installed libffi home.
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
            primary: "kirac".to_string(),
        };
        let text = current.to_toml();
        let parsed = CurrentToolchain::parse_toml(&text).unwrap();
        assert_eq!(current, parsed);
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
}
