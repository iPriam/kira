//! The `sync` command: bring `kira.lock` back in line with the manifests.
//!
//! Resolution already knows the answer — this verb is the one that writes it
//! down. `run`, `build`, and `check` sync a lockfile that has drifted as a side
//! effect of resolving one, so reaching for this verb is for the case where
//! that is the whole intent: after editing a manifest, or to create the
//! lockfile a project does not have yet.

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use kira_diagnostics::renderer;
use kira_package_manager::SyncOutcome;
use kira_source::SourceMap;
use std::path::Path;

/// Runs `kira sync [dir]`: resolve the package graph and write `kira.lock`.
///
/// With no path, syncs the package you are standing in — the same default
/// `run`, `build`, and `check` take.
pub fn sync(args: &[String]) -> i32 {
    let path = args
        .first()
        .map(String::as_str)
        .unwrap_or(crate::options::DEFAULT_PATH);
    let root = Path::new(path);
    let graph = match kira_package_manager::resolve(root) {
        Ok(graph) => graph,
        Err(error) => {
            eprintln!("kira sync: {error}");
            return EXIT_USAGE;
        }
    };

    // Resolution problems are shown before anything is written: a lockfile
    // pinned from a graph with a missing package would record the hole as if
    // it were the answer. An Error-severity diagnostic refuses the write and
    // fails the verb — a lockfile that blesses a broken graph would also
    // silence every future drift check, because it compares like-for-like.
    let sources = SourceMap::default();
    for diagnostic in &graph.diagnostics {
        eprintln!("{}", renderer::render(diagnostic, &sources));
    }
    if graph
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == kira_diagnostics::Severity::Error)
    {
        eprintln!("kira sync: resolve the errors above before syncing; nothing was written");
        return EXIT_FAILURE;
    }

    match kira_package_manager::sync_lockfile(root, &graph.packages) {
        Ok(SyncOutcome::Written) => {
            println!("synced: {}", root.join("kira.lock").display());
            EXIT_OK
        }
        Ok(SyncOutcome::Unchanged) => {
            println!("already current: {}", root.join("kira.lock").display());
            EXIT_OK
        }
        Err(error) => {
            eprintln!("kira sync: {error}");
            EXIT_FAILURE
        }
    }
}
