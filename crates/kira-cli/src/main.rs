//! The kira CLI: check, run, build, and test entry point wiring together all compiler and runtime crates.
//!
//! Layer 9 of the Kira package graph.

mod build_lock;
mod command;
mod compiler_host;
mod debugger;
mod dependencies;
mod diagnostics;
mod dispatch;
mod doc;
mod export;
mod export_apple;
mod ffi;
mod foreign_libs;
mod hybrid;
mod hybrid_launcher;
mod hybrid_library;
mod inspect;
mod library;
mod live;
mod live_apple;
mod live_scaffold;
mod live_web;
mod migrate;
mod native;
mod native_library;
mod options;
mod pipeline;
mod profile;
mod progress;
mod scaffold;
mod serve;
mod shader;
mod supervisor;
mod sync;
mod timings;
mod update;
mod wasm;

use command::Command;

fn main() {
    // A direct binding to the host process resolves out of `kira` itself, so
    // the exported runtime symbols must reach this binary's link graph.
    kira_native_bridge::retain_process_exports();
    // Granted before anything runs, because a program reaches the compiler from
    // whichever engine it happens to be on and both are started below.
    compiler_host::grant();
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("__hybrid-debug-host") => {
            kira_toolchain::process::exit(debugger::run_hybrid_host(&args[1..]));
        }
        Some("__vm-debug-host") => {
            kira_toolchain::process::exit(debugger::run_vm_host(&args[1..]));
        }
        _ => {}
    }
    let Some(verb) = args.first() else {
        // A bare `kira` is a request for orientation, not a mistake: it
        // prints the same screen `kira help` does and succeeds the same way.
        // Only an *unknown* verb below is a usage error.
        dispatch::print_usage();
        kira_toolchain::process::exit(0);
    };
    let Some(parsed) = Command::parse(verb) else {
        eprintln!("kira: unknown command '{verb}'");
        eprintln!();
        dispatch::print_usage();
        kira_toolchain::process::exit(dispatch::EXIT_UNAVAILABLE);
    };
    kira_toolchain::process::exit(dispatch::dispatch(parsed, &args[1..]));
}
