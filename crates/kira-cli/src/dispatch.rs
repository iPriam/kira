//! Verb dispatch: routes a parsed [`Command`] to its handler.
//!
//! Port target: kira-zig `kira_cli/src/app.zig` + `kira_cli/src/commands/`.
//! Every handler is a stub until its command ports; stubs exit 2.

use crate::command::{ALL, Command};

/// Exit code for not-yet-ported commands and usage errors.
pub const EXIT_UNAVAILABLE: i32 = 2;

/// Dispatch a parsed command. Returns the process exit code.
///
/// `_args` are the remaining CLI arguments after the verb; each handler
/// takes over their parsing when it ports.
pub fn dispatch(command: Command, _args: &[String]) -> i32 {
    match command {
        Command::Help => {
            print_usage();
            0
        }
        Command::Version => {
            println!(
                "kirac {} (rust port, scaffolding)",
                env!("CARGO_PKG_VERSION")
            );
            0
        }
        other => not_yet_ported(other),
    }
}

fn not_yet_ported(command: Command) -> i32 {
    eprintln!(
        "kirac {}: not yet ported from kira-zig (see kira-zig `kira_cli/src/commands/`)",
        command.label()
    );
    EXIT_UNAVAILABLE
}

/// Print top-level usage (internal verbs are hidden).
pub fn print_usage() {
    eprintln!("kirac — the Kira compiler CLI (Rust port, scaffolding)");
    eprintln!();
    eprintln!("usage: kirac <command> [args]");
    eprintln!();
    for kind in ALL {
        if !kind.is_internal() {
            eprintln!("  {}", kind.label());
        }
    }
}
