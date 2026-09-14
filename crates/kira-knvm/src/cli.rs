//! The `knvm` verbs and their parsing.
//!
//! Hand-rolled like `kira`'s — knvm is the first thing a user installs, so it
//! takes no argument-parsing dependency. Selection is a structured enum all the
//! way down: nothing downstream matches on a verb or a channel string.

use kira_toolchain::Channel;

use crate::binstall::BuildProfile;

/// Which version of a toolchain an invocation names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionSpec {
    /// The newest version published on the channel.
    Latest,
    /// One named version.
    Exact(String),
}

impl VersionSpec {
    /// Reads a version argument: the literal `latest`, or a version.
    #[must_use]
    pub fn parse(argument: &str) -> Self {
        if argument == "latest" {
            Self::Latest
        } else {
            Self::Exact(argument.to_string())
        }
    }
}

/// What `knvm llvm` does to the managed LLVM tree.
///
/// One verb owns every LLVM operation because they share a home the toolchain
/// verbs never touch: `<toolchains-root>/llvm/<version>/`, a versioned sibling
/// of the installed toolchains that a channel install must not write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LlvmAction {
    /// Report the pinned version and what is installed.
    Status,
    /// Download and install the pinned bundle for this host.
    Install {
        /// Whether to replace a bundle that is already installed.
        force: bool,
    },
    /// Remove one installed version's whole tree.
    Uninstall {
        /// The version to remove.
        version: String,
    },
}

/// A parsed `knvm` invocation.
///
/// A verb this enum accepts is a verb that runs: nothing here is a placeholder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnvmCommand {
    /// Install a toolchain and select it.
    Install {
        /// Which version to install.
        spec: VersionSpec,
        /// Which channel to install it from.
        channel: Channel,
    },
    /// Provision the pinned libffi archives the runtime links statically.
    InstallLibffi {
        /// Whether to replace archives that are already installed.
        force: bool,
    },
    /// Build the enclosing checkout and install it as the dev toolchain.
    Binstall {
        /// Which cargo profile the toolchain is built with.
        profile: BuildProfile,
    },
    /// Build `knvm` and `kira` from the enclosing checkout and put them on PATH.
    Sinstall,
    /// Report the locally installed toolchains.
    List,
    /// Report the versions published on every channel.
    ListRemote,
    /// Manage the LLVM bundle the native backend links.
    Llvm(LlvmAction),
    /// Replace the installed tools with the newest published build.
    SelfUpdate {
        /// Which channel to take the newest tools from.
        channel: Channel,
    },
    /// Pin the toolchain a directory tree uses.
    Pin {
        /// Which version to pin to.
        version: String,
        /// Which channel it is installed on.
        channel: Channel,
    },
    /// Remove a directory tree's pin.
    Unpin,
    /// Select an already-installed toolchain.
    Use {
        /// Which installed version to select.
        version: String,
        /// Which channel it is installed on.
        channel: Channel,
    },
    /// Remove an installed toolchain.
    Uninstall {
        /// Which installed version to remove.
        version: String,
        /// Which channel it is installed on.
        channel: Channel,
    },
    /// Print the usage text.
    Help,
    /// Bare `knvm`: greet, report what is installed and selected, hint next
    /// steps. Not an error — the tool introducing itself is the front door.
    Overview,
}

/// Why an invocation could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UsageError {
    /// The verb is not one knvm has.
    #[error("unknown command `{0}`")]
    UnknownCommand(String),
    /// `install` was given no version.
    #[error("`install` expects a version, or `latest`")]
    InstallMissingVersion,
    /// `use` or `uninstall` was given no version.
    ///
    /// Neither takes `latest`: they act on what is installed, and resolving
    /// `latest` is a question for a release feed, not for a local directory.
    #[error("`{0}` expects an installed version")]
    MissingVersion(&'static str),
    /// `--channel` was given without a value.
    #[error("`--channel` expects one of: release, dev")]
    ChannelMissingValue,
    /// `--channel` was given a value that is not a channel.
    #[error("unknown channel `{0}`; expected one of: release, dev")]
    UnknownChannel(String),
    /// A positional argument was given that the verb has no use for.
    #[error("unexpected argument `{0}`")]
    UnexpectedArgument(String),
    /// A flag was given that the verb has no use for.
    #[error("unknown option `{0}`")]
    UnknownOption(String),
}

/// The default channel, when `--channel` is not given.
pub const DEFAULT_CHANNEL: Channel = Channel::Release;

/// Parses an argument list, excluding the program name.
pub fn parse(arguments: &[String]) -> Result<KnvmCommand, UsageError> {
    let Some(verb) = arguments.first() else {
        return Ok(KnvmCommand::Overview);
    };
    let rest = &arguments[1..];

    match verb.as_str() {
        "help" | "--help" | "-h" => Ok(KnvmCommand::Help),
        "install" => {
            // `libffi` where a version goes is the engine, not a toolchain
            // called `libffi`. It is spelled as an argument to `install` rather
            // than as an `install-libffi` verb because it is a thing knvm
            // installs, and the reason LLVM has its own `llvm` verb — that it
            // is a versioned sibling with its own install, uninstall, and status
            // — does not apply: libffi's version is the pin and nothing selects
            // it, so it has only the one operation.
            if rest.first().is_some_and(|first| first == "libffi") {
                let force = match rest.get(1).map(String::as_str) {
                    None => false,
                    Some("--force") => {
                        reject_arguments(&rest[2..])?;
                        true
                    }
                    Some(extra) if extra.starts_with("--") => {
                        return Err(UsageError::UnknownOption(extra.to_string()));
                    }
                    Some(extra) => {
                        return Err(UsageError::UnexpectedArgument(extra.to_string()));
                    }
                };
                return Ok(KnvmCommand::InstallLibffi { force });
            }
            let parsed = parse_version_and_channel(rest)?;
            Ok(KnvmCommand::Install {
                spec: parsed
                    .version
                    .as_deref()
                    .map(VersionSpec::parse)
                    .ok_or(UsageError::InstallMissingVersion)?,
                channel: parsed.channel,
            })
        }
        "use" | "switch" => {
            let parsed = parse_version_and_channel(rest)?;
            Ok(KnvmCommand::Use {
                version: parsed.version.ok_or(UsageError::MissingVersion("use"))?,
                channel: parsed.channel,
            })
        }
        "uninstall" => {
            let parsed = parse_version_and_channel(rest)?;
            Ok(KnvmCommand::Uninstall {
                version: parsed
                    .version
                    .ok_or(UsageError::MissingVersion("uninstall"))?,
                channel: parsed.channel,
            })
        }
        "list" => {
            // `list` reports every channel at once, so it takes no `--channel`
            // and rejects one rather than silently ignoring it. `--remote` asks
            // the same question of the feed instead of of the disk, which is
            // why it is a flag on `list` and not a verb of its own.
            match rest.first().map(String::as_str) {
                None => Ok(KnvmCommand::List),
                Some("--remote") => {
                    reject_arguments(&rest[1..])?;
                    Ok(KnvmCommand::ListRemote)
                }
                Some(_) => {
                    reject_arguments(rest)?;
                    Ok(KnvmCommand::List)
                }
            }
        }
        "llvm" => parse_llvm(rest),
        "self-update" => {
            let parsed = parse_version_and_channel(rest)?;
            if let Some(version) = parsed.version {
                return Err(UsageError::UnexpectedArgument(version));
            }
            Ok(KnvmCommand::SelfUpdate {
                channel: parsed.channel,
            })
        }
        "pin" => {
            let parsed = parse_version_and_channel(rest)?;
            Ok(KnvmCommand::Pin {
                version: parsed.version.ok_or(UsageError::MissingVersion("pin"))?,
                channel: parsed.channel,
            })
        }
        "unpin" => {
            reject_arguments(rest)?;
            Ok(KnvmCommand::Unpin)
        }
        "binstall" => {
            // The checkout is found from the working directory and the channel
            // is always `dev`, so the profile is the only thing to configure.
            let profile = match rest.first().map(String::as_str) {
                None => BuildProfile::Release,
                Some("--debug") => {
                    reject_arguments(&rest[1..])?;
                    BuildProfile::Debug
                }
                Some(extra) if extra.starts_with("--") => {
                    return Err(UsageError::UnknownOption(extra.to_string()));
                }
                Some(extra) => return Err(UsageError::UnexpectedArgument(extra.to_string())),
            };
            Ok(KnvmCommand::Binstall { profile })
        }
        "sinstall" => {
            reject_arguments(rest)?;
            Ok(KnvmCommand::Sinstall)
        }
        other => Err(UsageError::UnknownCommand(other.to_string())),
    }
}

/// Parses `llvm [install [--force] | uninstall <version>]`.
///
/// A bare `llvm` reports status rather than erroring: it is the front door to
/// the verb, the way a bare `knvm` is to the tool. `install` takes the pinned
/// version and this host, so its only choice is whether to replace what is
/// there; `uninstall` names an installed version, which — unlike `install` —
/// is not the pin, because old versions are exactly what it removes.
fn parse_llvm(rest: &[String]) -> Result<KnvmCommand, UsageError> {
    match rest.first().map(String::as_str) {
        None => Ok(KnvmCommand::Llvm(LlvmAction::Status)),
        Some("install") => {
            let force = match rest.get(1).map(String::as_str) {
                None => false,
                Some("--force") => {
                    reject_arguments(&rest[2..])?;
                    true
                }
                Some(extra) if extra.starts_with("--") => {
                    return Err(UsageError::UnknownOption(extra.to_string()));
                }
                Some(extra) => return Err(UsageError::UnexpectedArgument(extra.to_string())),
            };
            Ok(KnvmCommand::Llvm(LlvmAction::Install { force }))
        }
        Some("uninstall") => {
            let version = rest
                .get(1)
                .ok_or(UsageError::MissingVersion("llvm uninstall"))?;
            if version.starts_with("--") {
                return Err(UsageError::UnknownOption(version.clone()));
            }
            reject_arguments(&rest[2..])?;
            Ok(KnvmCommand::Llvm(LlvmAction::Uninstall {
                version: version.clone(),
            }))
        }
        Some(other) => Err(UsageError::UnknownCommand(format!("llvm {other}"))),
    }
}

/// Rejects any argument to a verb that takes none.
fn reject_arguments(rest: &[String]) -> Result<(), UsageError> {
    match rest.first() {
        None => Ok(()),
        Some(extra) if extra.starts_with("--") => Err(UsageError::UnknownOption(extra.clone())),
        Some(extra) => Err(UsageError::UnexpectedArgument(extra.clone())),
    }
}

/// The argument shape every versioned verb shares.
struct VersionAndChannel {
    /// The positional version, if one was given.
    version: Option<String>,
    /// The channel, defaulted when `--channel` was absent.
    channel: Channel,
}

/// Parses `<version> [--channel <channel>]`, in either order.
fn parse_version_and_channel(arguments: &[String]) -> Result<VersionAndChannel, UsageError> {
    let mut version = None;
    let mut channel = DEFAULT_CHANNEL;

    let mut index = 0;
    while index < arguments.len() {
        let argument = arguments[index].as_str();
        match argument {
            "--channel" => {
                let value = arguments
                    .get(index + 1)
                    .ok_or(UsageError::ChannelMissingValue)?;
                channel = Channel::parse(value)
                    .ok_or_else(|| UsageError::UnknownChannel(value.clone()))?;
                index += 2;
            }
            _ if argument.starts_with("--") => {
                return Err(UsageError::UnknownOption(argument.to_string()));
            }
            _ if version.is_none() => {
                version = Some(argument.to_string());
                index += 1;
            }
            _ => return Err(UsageError::UnexpectedArgument(argument.to_string())),
        }
    }

    Ok(VersionAndChannel { version, channel })
}

/// The usage text, as one block.
#[must_use]
pub fn usage(paint: crate::Paint) -> String {
    // Invocation, arguments, one-line note. The note column is aligned by the
    // *visible* width of the invocation — padding is computed before color is
    // applied, because ANSI escapes inflate `len()` and would stagger it.
    const VERBS: [(&str, &str, &str); 11] = [
        (
            "install",
            " <version|latest> [--channel]",
            "fetch a release and select it",
        ),
        (
            "llvm",
            " [install [--force]|uninstall <version>]",
            "manage the LLVM the backend links",
        ),
        (
            "install libffi",
            " [--force]",
            "the libffi the runtime links in",
        ),
        (
            "binstall",
            " [--debug]",
            "this checkout as the dev toolchain",
        ),
        ("sinstall", "", "knvm and kira themselves, onto PATH"),
        ("self-update", " [--channel]", "the newest published tools"),
        ("list", " [--remote]", "what is installed, or published"),
        (
            "use",
            " <version> [--channel]",
            "select an installed version",
        ),
        (
            "pin",
            " <version> [--channel]",
            "pin this directory tree to a version",
        ),
        ("unpin", "", "remove this directory tree's pin"),
        (
            "uninstall",
            " <version> [--channel]",
            "remove an installed version",
        ),
    ];
    let width = VERBS
        .iter()
        .map(|(name, arguments, _)| "knvm ".len() + name.len() + arguments.len())
        .max()
        .unwrap_or(0);

    let mut text = format!(
        "{title} — the Kira version manager\n\n{usage}\n",
        title = paint.bold("knvm"),
        usage = paint.bold("Usage:")
    );
    for (name, arguments, note) in VERBS {
        let visible = "knvm ".len() + name.len() + arguments.len();
        text.push_str(&format!(
            "  {}{}{}   {}\n",
            paint.cyan(&format!("knvm {name}")),
            arguments,
            " ".repeat(width - visible),
            paint.dim(note),
        ));
    }
    text.push_str(&format!(
        "\n{options}\n\
         \x20 --channel <release|dev>   {channel_note}\n\
         \x20 --remote                  {remote_note}\n\
         \x20 --force                   {force_note}\n\
         \x20 -h, --help                {help_note}\n",
        options = paint.bold("Options:"),
        channel_note = paint.dim("which channel to act on (default: release)"),
        remote_note = paint.dim("list what is published rather than installed"),
        force_note = paint.dim("replace an already-installed LLVM bundle"),
        help_note = paint.dim("print this message"),
    ));
    text
}

#[cfg(test)]
mod tests;
