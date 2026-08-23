//! `kira update`: re-resolve the package graph and refresh `kira.lock`.
//!
//! The current resolver has complete local-path resolution and records registry
//! and git declarations as source pins. This command therefore performs the
//! same safe graph refresh as `sync`; network fetching is intentionally not
//! claimed until a registry/git resolver exists to provide a real new version.

use kira_diagnostics::renderer;
use kira_package_manager::SyncOutcome;
use kira_source::SourceMap;
use std::path::Path;

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::err;

/// Runs `kira update [dir]`.
pub fn update(args: &[String]) -> i32 {
    let root = match args {
        [] => Path::new(".").to_path_buf(),
        [path] if !path.starts_with('-') => Path::new(path).to_path_buf(),
        _ => {
            err!("kira update: expected at most one package directory");
            return EXIT_USAGE;
        }
    };
    let graph = match kira_package_manager::resolve(&root) {
        Ok(graph) => graph,
        Err(error) => {
            err!("kira update: {error}");
            return EXIT_USAGE;
        }
    };
    let sources = SourceMap::default();
    for diagnostic in &graph.diagnostics {
        eprintln!("{}", renderer::render(diagnostic, &sources));
    }
    // The same refusal `sync` makes: a lockfile pinned from a graph with a
    // missing package records the hole as if it were the answer.
    if graph
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == kira_diagnostics::Severity::Error)
    {
        err!("kira update: resolve the errors above before updating; nothing was written");
        return EXIT_FAILURE;
    }
    match kira_package_manager::sync_lockfile(&root, &graph.packages) {
        Ok(SyncOutcome::Written) => {
            println!("updated: {}", root.join("kira.lock").display());
            EXIT_OK
        }
        Ok(SyncOutcome::Unchanged) => {
            println!("already current: {}", root.join("kira.lock").display());
            EXIT_OK
        }
        Err(error) => {
            err!("kira update: {error}");
            EXIT_FAILURE
        }
    }
}
