use super::*;

fn parse_args(arguments: &[&str]) -> Result<KnvmCommand, UsageError> {
    let owned: Vec<String> = arguments.iter().map(|text| (*text).to_string()).collect();
    parse(&owned)
}

/// A dev toolchain is what every downstream project then compiles through,
/// so the plain verb has to produce the optimized one.
#[test]
fn binstall_builds_an_optimized_toolchain_unless_debug_is_asked_for() {
    assert_eq!(
        parse_args(&["binstall"]),
        Ok(KnvmCommand::Binstall {
            profile: BuildProfile::Release,
        })
    );
    assert_eq!(
        parse_args(&["binstall", "--debug"]),
        Ok(KnvmCommand::Binstall {
            profile: BuildProfile::Debug,
        })
    );
    assert_eq!(
        parse_args(&["binstall", "--fast"]),
        Err(UsageError::UnknownOption("--fast".to_string()))
    );
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
fn parses_the_llvm_verb_and_its_three_shapes() {
    // Bare `llvm` is the front door: status, not an error.
    assert_eq!(
        parse_args(&["llvm"]),
        Ok(KnvmCommand::Llvm(LlvmAction::Status))
    );
    assert_eq!(
        parse_args(&["llvm", "install"]),
        Ok(KnvmCommand::Llvm(LlvmAction::Install { force: false }))
    );
    assert_eq!(
        parse_args(&["llvm", "install", "--force"]),
        Ok(KnvmCommand::Llvm(LlvmAction::Install { force: true }))
    );
    // The install version is the compiled-in pin, never an argument.
    assert_eq!(
        parse_args(&["llvm", "install", "22.1.4"]),
        Err(UsageError::UnexpectedArgument("22.1.4".to_string()))
    );
    assert_eq!(
        parse_args(&["llvm", "uninstall", "22.1.4"]),
        Ok(KnvmCommand::Llvm(LlvmAction::Uninstall {
            version: "22.1.4".to_string(),
        }))
    );
    // Uninstall names an installed version, so it must be given one.
    assert_eq!(
        parse_args(&["llvm", "uninstall"]),
        Err(UsageError::MissingVersion("llvm uninstall"))
    );
    assert_eq!(
        parse_args(&["llvm", "uninstall", "--force"]),
        Err(UsageError::UnknownOption("--force".to_string()))
    );
    assert_eq!(
        parse_args(&["llvm", "reinstall"]),
        Err(UsageError::UnknownCommand("llvm reinstall".to_string()))
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
        "llvm",
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
