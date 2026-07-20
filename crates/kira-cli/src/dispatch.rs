//! Verb dispatch: routes a parsed [`Command`] to its handler.
//!
//! Every handler is a stub until its command is implemented; stubs exit 2.

use crate::command::{ALL, Command};
use crate::pipeline;

/// Exit code for unimplemented commands and usage errors.
pub const EXIT_UNAVAILABLE: i32 = 2;

/// Dispatch a parsed command. Returns the process exit code.
///
/// `args` are the remaining CLI arguments after the verb; each handler takes
/// over their parsing as it is implemented.
pub fn dispatch(command: Command, args: &[String]) -> i32 {
    match command {
        Command::Run => pipeline::run(args),
        Command::Build => pipeline::build(args),
        Command::Check => pipeline::check(args),
        Command::Live => pipeline::live(args),
        Command::Help => {
            let all = args.iter().any(|arg| arg == "all" || arg == "--all");
            print_usage_with(all);
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

/// Whether a verb has a real handler, or still hits the stub above.
///
/// Lives beside the `dispatch` match so the two cannot drift apart silently;
/// the usage screen dims what is not yet real rather than advertising every
/// verb as equally alive.
fn implemented(command: Command) -> bool {
    matches!(
        command,
        Command::Run
            | Command::Build
            | Command::Check
            | Command::Live
            | Command::Help
            | Command::Version
    )
}

/// Print top-level usage: the commands that work.
pub fn print_usage() {
    print_usage_with(false);
}

/// Print usage; `all` includes the verbs that are parsed but not yet real.
///
/// The default screen lists only what runs — a front door advertising sixteen
/// stubs reads as sixteen broken promises. The planned set stays reachable
/// (`kirac help all`) and stays honest: every hidden verb is labeled
/// unimplemented.
fn print_usage_with(all: bool) {
    let paint = kira_toolchain::Paint::auto_stderr();
    // The note column is aligned by the visible width of `kirac <verb><args>` —
    // padding is computed before color is applied, because ANSI escapes
    // inflate `len()`, and from the rows this screen actually shows, so hiding
    // the planned set does not pad the short list to the longest hidden name.
    let visible = |kind: &Command| "kirac ".len() + kind.label().len() + kind.arguments().len();
    let width = ALL
        .iter()
        .filter(|kind| all || implemented(**kind))
        .map(visible)
        .max()
        .unwrap_or(0)
        .max("kirac --version".len());

    eprintln!("{} — the Kira compiler CLI", paint.bold("kirac"));
    eprintln!();
    for kind in ALL.into_iter().filter(|kind| implemented(*kind)) {
        let pad = " ".repeat(width - visible(&kind));
        eprintln!(
            "  {}{}{pad}   {}",
            paint.cyan(&format!("kirac {}", kind.label())),
            kind.arguments(),
            paint.dim(kind.description())
        );
    }
    {
        let pad = " ".repeat(width - "kirac --version".len());
        eprintln!(
            "  {}{pad}   {}",
            paint.cyan("kirac --version"),
            paint.dim("print the version")
        );
    }

    let planned = ALL.into_iter().filter(|kind| !implemented(*kind));
    if all {
        eprintln!();
        for kind in planned {
            let pad = " ".repeat(width - visible(&kind));
            eprintln!(
                "  {}{}{pad}   {}",
                paint.dim(&format!("kirac {}", kind.label())),
                kind.arguments(),
                paint.dim(&format!("{} (not yet implemented)", kind.description()))
            );
        }
    } else {
        let count = planned.count();
        eprintln!();
        eprintln!(
            "{}",
            paint.dim(&format!(
                "{count} more commands are planned; `kirac help all` lists them."
            ))
        );
    }
}
