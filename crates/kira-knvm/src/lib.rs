//! knvm: the installer that provisions Kira toolchains into `~/.kira/toolchains`.
//!
//! Standalone tool crate at the binary layer, outside the layered package
//! graph — a leaf like `kira-launcher`, depending only on `kira-toolchain`
//! (layer 0).
//!
//! # Where the layout is defined
//!
//! Nowhere in this crate. Every managed path comes from `kira-toolchain`, which
//! already models `KIRA_HOME`, the channel namespace, and `current.toml`, and
//! whose `bundled_discovery` consumes what an install writes. knvm is what
//! *produces* the tree those functions describe; it does not get a second
//! opinion about its shape.
//!
//! # Why the logic lives in the library
//!
//! The binary is argv parsing and exit codes and nothing else. Install
//! orchestration lives here so integration tests drive the shipped code path
//! against a fixture release directory, rather than a parallel one written for
//! testing.

pub mod binstall;
pub mod cli;
pub mod github;
pub mod install;
pub mod manage;
pub mod sinstall;
pub mod source;

/// The layout vocabulary knvm produces trees for, re-exported so a consumer
/// needs one crate. These are `kira-toolchain`'s types, not copies of them.
pub use kira_toolchain::{Channel, CurrentToolchain, Paint};

pub use binstall::{BinstallError, binstall};
pub use cli::{DEFAULT_CHANNEL, KnvmCommand, UsageError, VersionSpec, usage};
pub use github::{
    DEFAULT_REPOSITORY, GitHubReleaseSource, ReleaseAsset, ReleaseEntry, parse_release_feed,
    releases_on_channel, select_asset, strip_tag_prefix,
};
pub use install::{
    InstallError, Installed, PRIMARY_BINARY, current_toolchain_path, install, read_current,
    toolchain_root, write_current,
};
pub use manage::{InstalledToolchain, ManageError, Selected, Uninstalled, list, select, uninstall};
pub use sinstall::{SelfInstalled, sinstall};
pub use source::{
    DirectoryReleaseSource, ReleaseSource, ReleaseSourceError, archive_file_name, compare_versions,
    current_host_key, sort_newest_first,
};
