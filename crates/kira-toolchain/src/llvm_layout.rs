//! Managed LLVM bundle directory layout (toolchains/llvm roots, host bundle keys).
//!
//! Merged from the kira-zig `kira_llvm_toolchain_layout` package; former
//! importers now depend on `kira-toolchain` and use this module. These are
//! the repo-local (`<repo>/.kira/toolchains/llvm/...`) layout paths; the
//! user-level (`~/.kira/...`) equivalents live in the crate root.

use std::path::{Path, PathBuf};

/// Directory name of the managed toolchains root under `.kira/`.
pub const MANAGED_TOOLCHAINS_DIR: &str = "toolchains";
/// Directory name of the managed LLVM bundles under the toolchains root.
pub const MANAGED_LLVM_DIR: &str = "llvm";

/// The LLVM bundle key for a host `(os, arch)` pair, matching the keys used
/// in `llvm-metadata.toml`. `os`/`arch` follow `std::env::consts` naming.
pub fn host_llvm_bundle_key(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("windows", "x86_64") => Some("x86_64-windows-msvc"),
        ("linux", "x86_64") => Some("x86_64-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-macos"),
        _ => None,
    }
}

/// The LLVM bundle key of the compiling host, if it is a supported host.
pub fn current_host_llvm_bundle_key() -> Option<&'static str> {
    host_llvm_bundle_key(std::env::consts::OS, std::env::consts::ARCH)
}

/// `<repo>/.kira/toolchains/llvm`.
pub fn managed_llvm_root(repo_root: &Path) -> PathBuf {
    repo_root
        .join(".kira")
        .join(MANAGED_TOOLCHAINS_DIR)
        .join(MANAGED_LLVM_DIR)
}

/// `<repo>/.kira/toolchains/llvm/<llvm-version>`.
pub fn managed_llvm_version_root(repo_root: &Path, llvm_version: &str) -> PathBuf {
    managed_llvm_root(repo_root).join(llvm_version)
}

/// `<repo>/.kira/toolchains/llvm/<llvm-version>/<host-key>` — an installed
/// repo-local LLVM home.
pub fn managed_llvm_home(repo_root: &Path, llvm_version: &str, host_key: &str) -> PathBuf {
    managed_llvm_version_root(repo_root, llvm_version).join(host_key)
}

/// `<repo>/.kira/llvm/current` — the legacy pre-toolchains LLVM location.
pub fn legacy_llvm_current_home(repo_root: &Path) -> PathBuf {
    repo_root.join(".kira").join("llvm").join("current")
}

/// `<repo>/.kira/llvm/llvm-<version>-<host-key>` — the legacy versioned
/// LLVM location.
pub fn legacy_llvm_versioned_home(repo_root: &Path, llvm_version: &str, host_key: &str) -> PathBuf {
    repo_root
        .join(".kira")
        .join("llvm")
        .join(format!("llvm-{llvm_version}-{host_key}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_supported_hosts_to_metadata_keys() {
        assert_eq!(
            Some("x86_64-windows-msvc"),
            host_llvm_bundle_key("windows", "x86_64")
        );
        assert_eq!(
            Some("x86_64-linux-gnu"),
            host_llvm_bundle_key("linux", "x86_64")
        );
        assert_eq!(
            Some("aarch64-macos"),
            host_llvm_bundle_key("macos", "aarch64")
        );
        assert_eq!(None, host_llvm_bundle_key("linux", "aarch64"));
    }

    #[test]
    fn builds_managed_install_path() {
        let path = managed_llvm_home(Path::new("/repo"), "21.1.2", "x86_64-linux-gnu");
        assert_eq!(
            PathBuf::from("/repo/.kira/toolchains/llvm/21.1.2/x86_64-linux-gnu"),
            path
        );
    }

    #[test]
    fn builds_legacy_paths() {
        assert_eq!(
            PathBuf::from("/repo/.kira/llvm/current"),
            legacy_llvm_current_home(Path::new("/repo"))
        );
        assert_eq!(
            PathBuf::from("/repo/.kira/llvm/llvm-21.1.2-aarch64-macos"),
            legacy_llvm_versioned_home(Path::new("/repo"), "21.1.2", "aarch64-macos")
        );
    }
}
