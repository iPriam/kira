//! Verb dispatch: routes a parsed [`Command`] to its handler.
//!
//! Every parsed command has an explicit handler; usage errors stay at the
//! handler boundary rather than falling through to a silent no-op.

use crate::command::{ALL, Command};
use crate::pipeline;

/// Exit code for unknown commands and usage errors.
pub const EXIT_UNAVAILABLE: i32 = 2;

/// Dispatch a parsed command. Returns the process exit code.
///
/// `args` are the remaining CLI arguments after the verb; each handler takes
/// over their parsing as it is implemented.
pub fn dispatch(command: Command, args: &[String]) -> i32 {
    match command {
        Command::Run => pipeline::run(args),
        Command::Debug => pipeline::debug(args),
        Command::Build => pipeline::build(args),
        Command::Package => pipeline::package(args),
        Command::Export => pipeline::export(args),
        Command::Check => pipeline::check(args),
        Command::Lint => pipeline::lint(args),
        Command::Test => pipeline::test(args),
        Command::Live => pipeline::live(args),
        Command::Tokens => crate::inspect::tokens(args),
        Command::Ast => crate::inspect::ast(args),
        Command::Doc => crate::doc::doc(args),
        Command::New => crate::scaffold::new(args),
        Command::Sync => crate::sync::sync(args),
        Command::Add => crate::dependencies::add(args),
        Command::Remove => crate::dependencies::remove(args),
        Command::MigrateManifest => crate::migrate::migrate(args),
        Command::Ffi => crate::ffi::ffi(args),
        Command::Instruments => crate::instruments::run(args),
        Command::Update => crate::update::update(args),
        Command::Shader => crate::shader::shader(args),
        Command::Help => {
            let all = args.iter().any(|arg| arg == "all" || arg == "--all");
            print_usage_with(all);
            0
        }
        Command::Version => {
            println!("kira {}", kira_toolchain::RELEASE_VERSION);
            0
        }
    }
}

/// Whether a verb has a real handler.
///
/// Kept beside the dispatch match so adding a parsed command forces its help
/// status to be considered at the same edit site.
fn implemented(command: Command) -> bool {
    matches!(
        command,
        Command::Run
            | Command::Debug
            | Command::Build
            | Command::Check
            | Command::Lint
            | Command::Test
            | Command::Live
            | Command::Tokens
            | Command::Ast
            | Command::Doc
            | Command::New
            | Command::Sync
            | Command::Add
            | Command::Remove
            | Command::MigrateManifest
            | Command::Ffi
            | Command::Instruments
            | Command::Update
            | Command::Package
            | Command::Export
            | Command::Shader
            | Command::Help
            | Command::Version
    )
}

/// Print top-level usage.
pub fn print_usage() {
    print_usage_with(false);
}

/// Print usage; `all` is retained for scripts that used the old help spelling.
///
/// All commands now have a handler, so both forms print the same command list.
fn print_usage_with(_all: bool) {
    let paint = kira_toolchain::Paint::auto_stderr();
    // The note column is aligned by the visible width of `kira <verb><args>` —
    // padding is computed before color is applied, because ANSI escapes
    // inflate `len()`.
    let visible = |kind: &Command| "kira ".len() + kind.label().len() + kind.arguments().len();
    let width = ALL
        .iter()
        .map(visible)
        .max()
        .unwrap_or(0)
        .max("kira --version".len());

    eprintln!("{} — the Kira compiler CLI", paint.bold("kira"));
    eprintln!();
    for kind in ALL.into_iter().filter(|kind| implemented(*kind)) {
        let pad = " ".repeat(width - visible(&kind));
        eprintln!(
            "  {}{}{pad}   {}",
            paint.cyan(&format!("kira {}", kind.label())),
            kind.arguments(),
            paint.dim(kind.description())
        );
    }
    {
        let pad = " ".repeat(width - "kira --version".len());
        eprintln!(
            "  {}{pad}   {}",
            paint.cyan("kira --version"),
            paint.dim("print the version")
        );
    }
}
