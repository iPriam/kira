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
        Verb::ReleaseWindow => release_window(),
        other => {
            eprintln!("devflow {}: not yet implemented", other.label());
            std::process::exit(2);
        }
    }
}

/// Print which version ships on which Tuesday, and how long the wait is.
fn release_window() {
    match ReleaseWindow::for_today() {
        Ok(window) => print!("{}", window.report()),
        Err(error) => {
            eprintln!("devflow release-window: {error}");
            std::process::exit(1);
        }
    }
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
