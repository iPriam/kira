//! The kira CLI: check, run, build, and test entry point wiring together all compiler and runtime crates.
//!
//! Layer 9 of the Kira package graph.

mod build_lock;
mod command;
mod compiler_host;
mod diagnostics;
mod dispatch;
mod foreign_libs;
mod hybrid;
mod hybrid_library;
mod library;
mod live;
mod native;
mod native_library;
mod options;
mod pipeline;
mod progress;
mod serve;
mod shader;
mod supervisor;
mod sync;
mod timings;
mod wasm;

use command::Command;

fn main() {
    // Granted before anything runs, because a program reaches the compiler from
    // whichever engine it happens to be on and both are started below.
    compiler_host::grant();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first() else {
        // A bare `kira` is a request for orientation, not a mistake: it
        // prints the same screen `kira help` does and succeeds the same way.
        // Only an *unknown* verb below is a usage error.
        dispatch::print_usage();
        std::process::exit(0);
    };
    let Some(parsed) = Command::parse(verb) else {
        eprintln!("kira: unknown command '{verb}'");
        eprintln!();
        dispatch::print_usage();
        std::process::exit(dispatch::EXIT_UNAVAILABLE);
    };
    std::process::exit(dispatch::dispatch(parsed, &args[1..]));
}
