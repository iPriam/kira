//! Repo-native automation for the fork -> PR -> review -> land -> sync flow.
//!
//! Standalone tool crate (outside the layered package graph).

mod commands;

use commands::Verb;

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
    eprintln!("devflow {}: not yet implemented", verb.label());
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
