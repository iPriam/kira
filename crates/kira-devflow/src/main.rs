//! Repo-native automation for the fork -> PR -> review -> land -> sync flow.
//!
//! Standalone tool crate (outside the layered package graph).

mod calendar;
mod commands;
mod release_window;

use commands::Verb;
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
    match verb {
        Verb::Status => status(&args[1..]),
        Verb::NextVersion => next_version(&args[1..]),
        Verb::ReleaseWindow => release_window(&args[1..]),
        other => {
            eprintln!("devflow {}: not yet implemented", other.label());
            std::process::exit(2);
        }
    }
}

/// Print which version ships on which Tuesday, and how long the wait is.
fn release_window(args: &[String]) {
    require_no_args("release-window", args);
    match ReleaseWindow::for_today() {
        Ok(window) => print!("{}", window.report()),
        Err(error) => {
            eprintln!("devflow release-window: {error}");
            std::process::exit(1);
        }
    }
}

/// Print the repository branch and worktree summary without changing it.
fn status(args: &[String]) {
    require_no_args("status", args);
    let output = match std::process::Command::new("git")
        .args(["status", "--short", "--branch"])
        .output()
    {
        Ok(output) => output,
        Err(error) => {
            eprintln!("devflow status: cannot run git: {error}");
            std::process::exit(1);
        }
    };
    if !output.status.success() {
        eprintln!(
            "devflow status: git failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
        std::process::exit(1);
    }
    print!("{}", String::from_utf8_lossy(&output.stdout));
}

/// Print the next scheduled release version only.
fn next_version(args: &[String]) {
    require_no_args("next-version", args);
    match ReleaseWindow::for_today() {
        Ok(window) => println!("{}", window.next.version),
        Err(error) => {
            eprintln!("devflow next-version: {error}");
            std::process::exit(1);
        }
    }
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
