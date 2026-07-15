//! Verb dispatch: routes a parsed [`Command`] to its handler.
//!
//! Every handler is a stub until its command is implemented; stubs exit 2.

use crate::command::{ALL, Command};

/// Exit code for unimplemented commands and usage errors.
pub const EXIT_UNAVAILABLE: i32 = 2;

/// Dispatch a parsed command. Returns the process exit code.
///
/// `_args` are the remaining CLI arguments after the verb; each handler
/// takes over their parsing as it is implemented.
pub fn dispatch(command: Command, _args: &[String]) -> i32 {
    match command {
        Command::Help => {
            print_usage();
            0
        }
        Command::Version => {
            println!("kirac {}", env!("CARGO_PKG_VERSION"));
            0
        }
        other => unavailable(other),
    }
}

fn unavailable(command: Command) -> i32 {
    eprintln!("kirac {}: not yet implemented", command.label());
    EXIT_UNAVAILABLE
}

/// Print top-level usage.
pub fn print_usage() {
    eprintln!("kirac — the Kira compiler CLI");
    eprintln!();
    eprintln!("usage: kirac <command> [args]");
    eprintln!();
    for kind in ALL {
        eprintln!("  {}", kind.label());
    }
}
