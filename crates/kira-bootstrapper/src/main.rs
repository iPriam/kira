//! The `kira` launcher: resolves the installed toolchain and dispatches to kirac.
//!
//! Standalone tool crate (outside the layered package graph). Resolution reads
//! `~/.kira/toolchains/current.toml` (see `kira-toolchain`) and executes the
//! selected primary binary; that dispatch is not yet implemented.

fn main() {
    eprintln!("kira: launcher not yet implemented");
    std::process::exit(2);
}
