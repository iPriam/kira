//! The `kira` launcher: resolves the installed toolchain and dispatches to kirac.
//!
//! Standalone tool crate (outside the layered package graph).
//! Ported from kira-zig `packages/kira_bootstrapper`. Resolution reads
//! `~/.kira/toolchains/current.toml` (see `kira-toolchain`) and executes the
//! selected primary binary; that dispatch lands with the port.

mod release_install;

fn main() {
    // Port pending: load kira_toolchain::current_toolchain_path(), parse
    // CurrentToolchain, and exec managed_primary_binary_path(...).
    eprintln!("kira: not yet ported from kira-zig");
    std::process::exit(2);
}
