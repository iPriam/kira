//! Per-project toolchain pinning: `kira-toolchain.toml` beside a project.
//!
//! `current.toml` is one global selection, which is the wrong granularity for
//! a machine that builds two projects wanting different compilers. A pin file
//! names a toolchain for a directory tree, and the launcher prefers it over
//! the global selection whenever one is found at or above the working
//! directory.
//!
//! # The file
//!
//! ```toml
//! channel = "release"   # optional; defaults to release
//! version = "1.10.0"
//! ```
//!
//! Deliberately the same two fields `current.toml` carries, minus `primary`:
//! a pin says which toolchain, never which binary inside it — that is the
//! launcher's own decision, made from `argv[0]`.
//!
//! # Why it is not searched for beyond the tree
//!
//! The walk stops at the filesystem root, and a pin found there would govern
//! every directory on the machine. Nothing prevents that spelling; it is
//! simply what the user asked for, the same way a `rust-toolchain.toml` in
//! `$HOME` is.

use std::path::{Path, PathBuf};

use crate::Channel;

/// The file name a directory tree's pin is written to.
pub const PIN_FILE_NAME: &str = "kira-toolchain.toml";

/// A toolchain pinned to a directory tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedToolchain {
    /// The channel the pinned version is installed on.
    pub channel: Channel,
    /// The pinned version.
    pub version: String,
    /// The file this was read from, so a diagnostic can name it.
    pub path: PathBuf,
}

/// Why a pin file could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// The file exists but is not the documented shape.
    #[error(
        "`{}` is not a toolchain pin: expected `version = \"<version>\"` and an \
         optional `channel = \"release\"|\"dev\"`",
        .path.display()
    )]
    Malformed {
        /// The file that could not be read as a pin.
        path: PathBuf,
    },
    /// The file names a channel that does not exist.
    #[error("`{}` names an unknown channel `{channel}`", .path.display())]
    UnknownChannel {
        /// The file that named it.
        path: PathBuf,
        /// The channel name it carried.
        channel: String,
    },
}

/// The on-disk shape of a pin file.
#[derive(serde::Deserialize)]
struct RawPin {
    /// The pinned version.
    version: String,
    /// The channel, defaulted to `release` when absent.
    channel: Option<String>,
}

impl PinnedToolchain {
    /// Serializes a pin, as `pin` writes it.
    #[must_use]
    pub fn to_toml(&self) -> String {
        format!(
            "# Written by `knvm pin`: the Kira toolchain this directory tree uses.\n\
             channel = \"{}\"\n\
             version = \"{}\"\n",
            self.channel.dir_name(),
            self.version,
        )
    }

    /// Reads a pin from a file's contents.
    pub fn parse_toml(path: &Path, contents: &str) -> Result<Self, PinError> {
        let raw: RawPin = toml::from_str(contents).map_err(|_| PinError::Malformed {
            path: path.to_path_buf(),
        })?;
        let channel = match raw.channel.as_deref() {
            None => Channel::Release,
            Some(name) => Channel::parse(name).ok_or_else(|| PinError::UnknownChannel {
                path: path.to_path_buf(),
                channel: name.to_string(),
            })?,
        };
        Ok(Self {
            channel,
            version: raw.version,
            path: path.to_path_buf(),
        })
    }
}

/// The pin governing `start`, found by walking up from it.
///
/// The nearest pin wins, so a pinned subproject inside a pinned repository
/// gets its own. `Ok(None)` means no pin was found anywhere above `start`,
/// which is the ordinary case and not an error. A pin that exists and cannot
/// be read *is* an error: silently falling back to the global selection would
/// run a different compiler than the one the file asked for, which is the one
/// outcome a pin exists to prevent.
pub fn find_pin(start: &Path) -> Result<Option<PinnedToolchain>, PinError> {
    for directory in start.ancestors() {
        let path = directory.join(PIN_FILE_NAME);
        let contents = match std::fs::read_to_string(&path) {
            Ok(contents) => contents,
            // Unreadable for any reason other than absence — a permission
            // denial, a directory of that name — is treated as no pin here:
            // the walk continues, and a pin that cannot be opened at all is
            // not a pin this process can honor or report on meaningfully.
            Err(_) => continue,
        };
        return PinnedToolchain::parse_toml(&path, &contents).map(Some);
    }
    Ok(None)
}

/// Writes a pin into `directory`, returning the file it wrote.
pub fn write_pin(directory: &Path, pin: &PinnedToolchain) -> Result<PathBuf, std::io::Error> {
    let path = directory.join(PIN_FILE_NAME);
    std::fs::write(&path, pin.to_toml())?;
    Ok(path)
}

/// Removes the pin in `directory`, reporting whether there was one.
pub fn remove_pin(directory: &Path) -> Result<bool, std::io::Error> {
    let path = directory.join(PIN_FILE_NAME);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_tree(label: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("kira_pin_{label}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create the temp tree");
        path
    }

    #[test]
    fn round_trips_a_pin() {
        let pin = PinnedToolchain {
            channel: Channel::Dev,
            version: "2026.07.2".to_string(),
            path: PathBuf::from("/somewhere/kira-toolchain.toml"),
        };
        let parsed = PinnedToolchain::parse_toml(&pin.path, &pin.to_toml()).expect("its own text");
        assert_eq!(parsed, pin);
    }

    #[test]
    fn defaults_the_channel_to_release() {
        let path = Path::new("/p/kira-toolchain.toml");
        let parsed = PinnedToolchain::parse_toml(path, "version = \"1.10.0\"\n")
            .expect("a version alone is a pin");
        assert_eq!(parsed.channel, Channel::Release);
        assert_eq!(parsed.version, "1.10.0");
    }

    #[test]
    fn refuses_a_file_that_is_not_a_pin() {
        let path = Path::new("/p/kira-toolchain.toml");
        assert!(matches!(
            PinnedToolchain::parse_toml(path, "channel = \"release\"\n"),
            Err(PinError::Malformed { .. }),
        ));
        assert!(matches!(
            PinnedToolchain::parse_toml(path, "not toml at all {{{"),
            Err(PinError::Malformed { .. }),
        ));
        assert!(matches!(
            PinnedToolchain::parse_toml(path, "version = \"1.0\"\nchannel = \"nightly\"\n"),
            Err(PinError::UnknownChannel { .. }),
        ));
    }

    #[test]
    fn finds_the_nearest_pin_walking_up() {
        let root = temp_tree("nearest");
        let inner = root.join("packages").join("app");
        std::fs::create_dir_all(&inner).expect("create the nested tree");

        write_pin(
            &root,
            &PinnedToolchain {
                channel: Channel::Release,
                version: "1.10.0".to_string(),
                path: PathBuf::new(),
            },
        )
        .expect("write the outer pin");

        let found = find_pin(&inner)
            .expect("a readable pin")
            .expect("the outer pin governs the inner directory");
        assert_eq!(found.version, "1.10.0");

        write_pin(
            &inner,
            &PinnedToolchain {
                channel: Channel::Dev,
                version: "2026.07.2".to_string(),
                path: PathBuf::new(),
            },
        )
        .expect("write the inner pin");

        let found = find_pin(&inner).expect("a readable pin").expect("a pin");
        assert_eq!(
            (found.channel, found.version.as_str()),
            (Channel::Dev, "2026.07.2"),
            "the nearest pin must win"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reports_no_pin_rather_than_failing_when_there_is_none() {
        let root = temp_tree("nopin");
        assert_eq!(find_pin(&root).expect("no pin is not an error"), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn removing_reports_whether_there_was_one() {
        let root = temp_tree("remove");
        assert!(!remove_pin(&root).expect("removing nothing is not an error"));
        write_pin(
            &root,
            &PinnedToolchain {
                channel: Channel::Release,
                version: "1.10.0".to_string(),
                path: PathBuf::new(),
            },
        )
        .expect("write a pin");
        assert!(remove_pin(&root).expect("removing a pin"));
        assert_eq!(find_pin(&root).expect("gone"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
