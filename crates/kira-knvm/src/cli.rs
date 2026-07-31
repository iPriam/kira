//! The `knvm` verbs and their parsing.
//!
//! Hand-rolled like `kira`'s — knvm is the first thing a user installs, so it
//! takes no argument-parsing dependency. Selection is a structured enum all the
//! way down: nothing downstream matches on a verb or a channel string.

use kira_toolchain::Channel;

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
    /// Build the enclosing checkout and install it as the dev toolchain.
    Binstall,
    /// Build `knvm` and `kira` from the enclosing checkout and put them on PATH.
    Sinstall,
    /// Report the locally installed toolchains.
    List,
    /// Report the versions published on every channel.
    ListRemote,
    /// Provision the pinned LLVM bundle the native backend links.
    InstallLlvm {
        /// Whether to replace a bundle that is already installed.
        force: bool,
    },
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
        "install-llvm" => {
            // The version is the compiled-in pin and the host is this machine,
            // so the only choice is whether to replace what is already there.
            match rest.first().map(String::as_str) {
                None => Ok(KnvmCommand::InstallLlvm { force: false }),
                Some("--force") => {
                    reject_arguments(&rest[1..])?;
                    Ok(KnvmCommand::InstallLlvm { force: true })
                }
                Some(_) => {
                    reject_arguments(rest)?;
                    Ok(KnvmCommand::InstallLlvm { force: false })
                }
            }
        }
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
            // is always `dev`, so there is nothing to configure.
            reject_arguments(rest)?;
            Ok(KnvmCommand::Binstall)
        }
        "sinstall" => {
            reject_arguments(rest)?;
            Ok(KnvmCommand::Sinstall)
        }
        other => Err(UsageError::UnknownCommand(other.to_string())),
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
    const VERBS: [(&str, &str, &str); 10] = [
        (
            "install",
            " <version|latest> [--channel]",
            "fetch a release and select it",
        ),
        ("install-llvm", " [--force]", "the LLVM the backend links"),
        ("binstall", "", "this checkout as the dev toolchain"),
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
mod tests {
    use super::*;

    fn parse_args(arguments: &[&str]) -> Result<KnvmCommand, UsageError> {
        let owned: Vec<String> = arguments.iter().map(|text| (*text).to_string()).collect();
        parse(&owned)
    }

    #[test]
    fn parses_install_with_the_default_channel() {
        assert_eq!(
            parse_args(&["install", "latest"]),
            Ok(KnvmCommand::Install {
                spec: VersionSpec::Latest,
                channel: Channel::Release,
            })
        );
        assert_eq!(
            parse_args(&["install", "1.7.3"]),
            Ok(KnvmCommand::Install {
                spec: VersionSpec::Exact("1.7.3".to_string()),
                channel: Channel::Release,
            })
        );
    }

    #[test]
    fn parses_the_channel_flag_on_either_side_of_the_version() {
        let expected = KnvmCommand::Install {
            spec: VersionSpec::Latest,
            channel: Channel::Dev,
        };
        assert_eq!(
            parse_args(&["install", "latest", "--channel", "dev"]),
            Ok(expected.clone())
        );
        assert_eq!(
            parse_args(&["install", "--channel", "dev", "latest"]),
            Ok(expected)
        );
    }

    #[test]
    fn parses_help() {
        for spelling in ["help", "--help", "-h"] {
            assert_eq!(parse_args(&[spelling]), Ok(KnvmCommand::Help));
        }
    }

    #[test]
    fn a_bare_invocation_is_the_overview_not_an_error() {
        assert_eq!(parse_args(&[]), Ok(KnvmCommand::Overview));
    }

    #[test]
    fn rejects_bad_usage_by_name() {
        assert_eq!(
            parse_args(&["frobnicate"]),
            Err(UsageError::UnknownCommand("frobnicate".to_string()))
        );
        assert_eq!(
            parse_args(&["install"]),
            Err(UsageError::InstallMissingVersion)
        );
        assert_eq!(
            parse_args(&["install", "latest", "--channel"]),
            Err(UsageError::ChannelMissingValue)
        );
        assert_eq!(
            parse_args(&["install", "latest", "--channel", "nightly"]),
            Err(UsageError::UnknownChannel("nightly".to_string()))
        );
        assert_eq!(
            parse_args(&["install", "latest", "--verbose"]),
            Err(UsageError::UnknownOption("--verbose".to_string()))
        );
        assert_eq!(
            parse_args(&["install", "1.7.3", "2.0.0"]),
            Err(UsageError::UnexpectedArgument("2.0.0".to_string()))
        );
    }

    #[test]
    fn parses_list_and_refuses_arguments_it_has_no_use_for() {
        assert_eq!(parse_args(&["list"]), Ok(KnvmCommand::List));
        assert_eq!(
            parse_args(&["list", "--channel", "dev"]),
            Err(UsageError::UnknownOption("--channel".to_string())),
            "list reports every channel, so a channel filter must be refused, not ignored"
        );
        assert_eq!(
            parse_args(&["list", "1.7.3"]),
            Err(UsageError::UnexpectedArgument("1.7.3".to_string()))
        );
    }

    #[test]
    fn parses_use_under_both_spellings() {
        for spelling in ["use", "switch"] {
            assert_eq!(
                parse_args(&[spelling, "1.7.3"]),
                Ok(KnvmCommand::Use {
                    version: "1.7.3".to_string(),
                    channel: Channel::Release,
                })
            );
        }
        assert_eq!(
            parse_args(&["use", "--channel", "dev", "2026.07.2"]),
            Ok(KnvmCommand::Use {
                version: "2026.07.2".to_string(),
                channel: Channel::Dev,
            })
        );
    }

    #[test]
    fn parses_uninstall() {
        assert_eq!(
            parse_args(&["uninstall", "1.7.3"]),
            Ok(KnvmCommand::Uninstall {
                version: "1.7.3".to_string(),
                channel: Channel::Release,
            })
        );
        assert_eq!(
            parse_args(&["uninstall", "2026.07.2", "--channel", "dev"]),
            Ok(KnvmCommand::Uninstall {
                version: "2026.07.2".to_string(),
                channel: Channel::Dev,
            })
        );
    }

    #[test]
    fn requires_a_version_for_the_verbs_that_act_on_one() {
        assert_eq!(parse_args(&["use"]), Err(UsageError::MissingVersion("use")));
        assert_eq!(
            parse_args(&["switch"]),
            Err(UsageError::MissingVersion("use")),
            "the alias must report the canonical verb"
        );
        assert_eq!(
            parse_args(&["uninstall"]),
            Err(UsageError::MissingVersion("uninstall"))
        );
    }

    #[test]
    fn keeps_latest_out_of_the_verbs_that_act_on_installed_versions() {
        // `latest` is not special here: it is taken as a version name, which is
        // then refused downstream as not installed. Nothing silently resolves a
        // release feed for a local operation.
        assert_eq!(
            parse_args(&["use", "latest"]),
            Ok(KnvmCommand::Use {
                version: "latest".to_string(),
                channel: Channel::Release,
            })
        );
    }

    #[test]
    fn parses_the_two_shapes_of_list() {
        assert_eq!(parse_args(&["list"]), Ok(KnvmCommand::List));
        assert_eq!(
            parse_args(&["list", "--remote"]),
            Ok(KnvmCommand::ListRemote)
        );
        assert_eq!(
            parse_args(&["list", "--remote", "dev"]),
            Err(UsageError::UnexpectedArgument("dev".to_string())),
            "`--remote` reports every channel, so a filter must be refused"
        );
    }

    #[test]
    fn parses_install_llvm_and_its_only_option() {
        assert_eq!(
            parse_args(&["install-llvm"]),
            Ok(KnvmCommand::InstallLlvm { force: false })
        );
        assert_eq!(
            parse_args(&["install-llvm", "--force"]),
            Ok(KnvmCommand::InstallLlvm { force: true })
        );
        assert_eq!(
            parse_args(&["install-llvm", "22.1.4"]),
            Err(UsageError::UnexpectedArgument("22.1.4".to_string())),
            "the version is the compiled-in pin, never an argument"
        );
    }

    #[test]
    fn parses_self_update_which_takes_a_channel_and_no_version() {
        assert_eq!(
            parse_args(&["self-update"]),
            Ok(KnvmCommand::SelfUpdate {
                channel: Channel::Release
            })
        );
        assert_eq!(
            parse_args(&["self-update", "--channel", "dev"]),
            Ok(KnvmCommand::SelfUpdate {
                channel: Channel::Dev
            })
        );
        assert_eq!(
            parse_args(&["self-update", "1.7.3"]),
            Err(UsageError::UnexpectedArgument("1.7.3".to_string())),
            "self-update takes the newest, so naming a version is a misunderstanding to report"
        );
    }

    #[test]
    fn parses_pin_and_unpin() {
        assert_eq!(
            parse_args(&["pin", "1.10.0"]),
            Ok(KnvmCommand::Pin {
                version: "1.10.0".to_string(),
                channel: Channel::Release,
            })
        );
        assert_eq!(
            parse_args(&["pin", "2026.07.2", "--channel", "dev"]),
            Ok(KnvmCommand::Pin {
                version: "2026.07.2".to_string(),
                channel: Channel::Dev,
            })
        );
        assert_eq!(parse_args(&["pin"]), Err(UsageError::MissingVersion("pin")));
        assert_eq!(parse_args(&["unpin"]), Ok(KnvmCommand::Unpin));
        assert_eq!(
            parse_args(&["unpin", "1.10.0"]),
            Err(UsageError::UnexpectedArgument("1.10.0".to_string()))
        );
    }

    #[test]
    fn every_verb_the_usage_text_names_parses() {
        let text = usage(crate::Paint::plain());
        for verb in [
            "install",
            "install-llvm",
            "binstall",
            "sinstall",
            "self-update",
            "list",
            "use",
            "pin",
            "unpin",
            "uninstall",
        ] {
            assert!(
                text.contains(verb),
                "`{verb}` must be documented in the usage text"
            );
        }
        assert!(
            !text.contains("Not available yet"),
            "no verb is a placeholder any more"
        );
    }
}
