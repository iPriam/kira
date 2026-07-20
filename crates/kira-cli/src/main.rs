//! The kirac CLI: check, run, build, and test entry point wiring together all compiler and runtime crates.
//!
//! Layer 9 of the Kira package graph.

mod command;
mod dispatch;
mod hybrid;
mod hybrid_library;
mod library;
mod live;
mod native;
mod native_library;
mod options;
mod pipeline;
mod serve;
mod supervisor;
mod wasm;

use command::Command;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(verb) = args.first() else {
        // A bare `kirac` is a request for orientation, not a mistake: it
        // prints the same screen `kirac help` does and succeeds the same way.
        // Only an *unknown* verb below is a usage error.
        dispatch::print_usage();
        std::process::exit(0);
    };
    let Some(parsed) = Command::parse(verb) else {
        eprintln!("kirac: unknown command '{verb}'");
        eprintln!();
        dispatch::print_usage();
        std::process::exit(dispatch::EXIT_UNAVAILABLE);
    };
    std::process::exit(dispatch::dispatch(parsed, &args[1..]));
}
