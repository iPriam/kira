//! Repo-native automation for the fork -> PR -> AI review -> land -> sync flow.
//!
//! Standalone tool crate (outside the layered package graph). Each verb bakes
//! in a guard that a skill-only approach would leave to agent discretion:
//! push is always over the fork SSH remote, PR metadata always comes from the
//! complete `base...HEAD` branch, review gates require SUBMITTED bot reviews on
//! the exact head, and land is always a squash-as-PR followed by a resync.

mod calendar;
mod commands;
mod commit_msg;
mod context;
mod error;
mod gh_ops;
mod git_ops;
mod land_gates;
mod pr_scope;
mod proc;
mod release_window;

use commands::Verb;
use context::Context;
use error::DevflowError;
use release_window::ReleaseWindow;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(raw) = args.first() else {
        usage();
        std::process::exit(2);
    };
    let Some(verb) = Verb::parse(raw) else {
        eprintln!("devflow: unknown verb '{raw}'");
        eprintln!();
        usage();
        std::process::exit(2);
    };
    if let Err(error) = dispatch(verb, &args[1..]) {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

/// Route a parsed verb to its implementation.
fn dispatch(verb: Verb, rest: &[String]) -> Result<(), DevflowError> {
    match verb {
        // Release scheduling stays local; kira cuts releases through its own
        // scheme (see `release_window`), not the fork/upstream flow verbs.
        Verb::ReleaseWindow => {
            require_no_args("release-window", rest);
            release_window()
        }
        Verb::NextVersion => {
            require_no_args("next-version", rest);
            next_version()
        }
        Verb::ReleasePrep | Verb::Release => {
            eprintln!(
                "devflow {}: kira ships releases through its own release workflow; not a devflow verb",
                verb.label()
            );
            std::process::exit(2);
        }

        // Fork / upstream / PR / AI-review flow.
        Verb::Status => commands::status(&Context::discover()?),
        Verb::Commit => commands::commit(&Context::discover()?, flag_value(rest, "-m")),
        Verb::Push => commands::push(&Context::discover()?),
        Verb::PrScope => commands::pr_scope(&Context::discover()?),
        Verb::OpenForkPr => commands::open_fork_pr(&Context::discover()?),
        Verb::RequestReviews => commands::request_reviews(
            &Context::discover()?,
            require_number(rest),
            has_flag(rest, "--codex"),
        ),
        Verb::WaitCi => commands::wait_ci(&Context::discover()?, require_number(rest)),
        Verb::CiFailures => commands::ci_failures(&Context::discover()?, require_number(rest)),
        Verb::CiRunners => commands::ci_runners(&Context::discover()?, require_number(rest)),
        Verb::RerunCi => commands::rerun_ci(&Context::discover()?, require_number(rest)),
        Verb::ReviewFindings => commands::review_findings(
            &Context::discover()?,
            require_number(rest),
            has_flag(rest, "--codex"),
        ),
        Verb::WaitReviews => commands::wait_reviews(
            &Context::discover()?,
            require_number(rest),
            has_flag(rest, "--codex"),
        ),
        Verb::ResolveThread => {
            let number = require_number(rest);
            let Some(target) = second_positional(rest) else {
                eprintln!("devflow: resolve-thread requires <pr> <path>:<line>");
                std::process::exit(2);
            };
            let Some(body) = flag_value(rest, "-m") else {
                eprintln!(
                    "devflow: resolve-thread requires -m \"reason\" (what addressed the finding, or why it was rejected)"
                );
                std::process::exit(2);
            };
            commands::resolve_thread(&Context::discover()?, number, target, body)
        }
        Verb::Land => commands::land(
            &Context::discover()?,
            require_number(rest),
            has_flag(rest, "--codex"),
            has_flag(rest, "--force"),
        ),
        Verb::Sync => commands::sync(&Context::discover()?),
        Verb::OpenUpstreamPr => commands::open_upstream_pr(&Context::discover()?),
    }
}

/// Print which version ships on which Tuesday, and how long the wait is.
fn release_window() -> Result<(), DevflowError> {
    let window = ReleaseWindow::for_today()
        .map_err(|error| DevflowError::msg(format!("release-window: {error}")))?;
    print!("{}", window.report());
    Ok(())
}

/// Print the next scheduled release version only.
fn next_version() -> Result<(), DevflowError> {
    let window = ReleaseWindow::for_today()
        .map_err(|error| DevflowError::msg(format!("next-version: {error}")))?;
    println!("{}", window.next.version);
    Ok(())
}

/// The first bare positional (a non-flag argument), if any.
fn positional(rest: &[String]) -> Option<&str> {
    rest.iter()
        .map(String::as_str)
        .find(|arg| !arg.starts_with('-'))
}

/// The second bare positional, skipping a value-taking flag (`-m`) and its
/// value so a message body is never mistaken for a positional.
fn second_positional(rest: &[String]) -> Option<&str> {
    let mut seen_first = false;
    let mut index = 0;
    while index < rest.len() {
        let arg = rest[index].as_str();
        if arg == "-m" {
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        if !seen_first {
            seen_first = true;
            index += 1;
            continue;
        }
        return Some(arg);
    }
    None
}

/// Parse the PR number this verb requires, or fail with exit code 2.
fn require_number(rest: &[String]) -> u32 {
    let Some(text) = positional(rest) else {
        eprintln!("devflow: this verb requires a PR number");
        std::process::exit(2);
    };
    match text.parse::<u32>() {
        Ok(number) => number,
        Err(_) => {
            eprintln!("devflow: invalid PR number \"{text}\"");
            std::process::exit(2);
        }
    }
}

/// Whether `name` appears as a bare flag in `rest`.
fn has_flag(rest: &[String], name: &str) -> bool {
    rest.iter().any(|arg| arg == name)
}

/// The value following `name` (e.g. `-m "subject"`), or `None`.
fn flag_value<'a>(rest: &'a [String], name: &str) -> Option<&'a str> {
    rest.iter()
        .position(|arg| arg == name)
        .and_then(|index| rest.get(index + 1))
        .map(String::as_str)
}

/// Reject positional arguments for the read-only commands with one stable code.
fn require_no_args(verb: &str, args: &[String]) {
    if args.is_empty() {
        return;
    }
    eprintln!("devflow {verb}: expected no arguments");
    std::process::exit(2);
}

fn usage() {
    eprintln!("devflow — fork/upstream PR flow automation");
    eprintln!();
    eprintln!("usage: devflow <verb> [args]");
    eprintln!();
    for verb in commands::ALL {
        eprintln!("  {}", verb.label());
    }
}
