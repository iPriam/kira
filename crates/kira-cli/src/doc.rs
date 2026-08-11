//! `kira doc`: render documented declarations as Markdown.
//!
//! Documentation uses the parser directly, so it can describe a source file
//! even when the rest of the compiler is not available. A library directory is
//! expanded through the same discovery rules as a build and every package
//! source contributes to one deterministic document.

use std::path::{Path, PathBuf};

use kira_diagnostics::has_errors;
use kira_parser::parse;
use kira_project::{DiscoveryError, TargetKind};
use kira_source::SourceMap;

use crate::diagnostics;
use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Runs `kira doc [file|dir]`.
pub fn doc(args: &[String]) -> i32 {
    let path = match args {
        [] => PathBuf::from(crate::options::DEFAULT_PATH),
        [path] if !path.starts_with('-') => PathBuf::from(path),
        _ => {
            err!("kira doc: expected at most one source file or package directory");
            return EXIT_USAGE;
        }
    };
    let target = match kira_project::resolve_target(&path) {
        Ok(target) => target,
        Err(error) => {
            err!("kira doc: {error}");
            return discovery_exit(&error);
        }
    };
    let source_paths = match source_paths(&path, &target) {
        Ok(paths) => paths,
        Err(code) => return code,
    };
    let package = target
        .project_name
        .or_else(|| {
            path.file_stem()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "Kira".to_owned());

    let mut sources = SourceMap::new();
    let mut items = Vec::new();
    let mut parse_errors = false;
    for source_path in source_paths {
        let display = source_path.display().to_string();
        let text = match std::fs::read_to_string(&source_path) {
            Ok(text) => text,
            Err(error) => {
                err!("kira doc: cannot read `{display}`: {error}");
                return EXIT_USAGE;
            }
        };
        let source = match sources.insert(display, text.clone()) {
            Ok(source) => source,
            Err(error) => {
                err!("kira doc: cannot register source: {error}");
                return EXIT_FAILURE;
            }
        };
        let parsed = parse(source, &text);
        parse_errors |= has_errors(&parsed.diagnostics);
        items.extend(kira_doc::collect(source, &text, &parsed));
        diagnostics::emit(&parsed.diagnostics, &sources);
    }

    out!("{}", kira_doc::render_markdown(&package, &items));
    if parse_errors { EXIT_FAILURE } else { EXIT_OK }
}

/// Expands a library directory to every source owned by that package.
fn source_paths(path: &Path, target: &kira_project::ResolvedTarget) -> Result<Vec<PathBuf>, i32> {
    if target.target_kind == TargetKind::Library && path.is_dir() {
        let manifest = match kira_project::manifest_for(path) {
            Ok(Some(manifest)) => manifest,
            Ok(None) => {
                err!("kira doc: no package manifest for `{}`", path.display());
                return Err(EXIT_USAGE);
            }
            Err(error) => {
                err!("kira doc: {error}");
                return Err(discovery_exit(&error));
            }
        };
        return match kira_project::library_sources(&manifest) {
            Ok(sources) => Ok(sources
                .iter()
                .map(|source| source.path().to_owned())
                .collect()),
            Err(error) => {
                err!("kira doc: {error}");
                Err(discovery_exit(&error))
            }
        };
    }
    target
        .source_path
        .as_deref()
        .map(PathBuf::from)
        .map(|path| vec![path])
        .ok_or_else(|| {
            err!("kira doc: `{}` did not resolve to a source", path.display());
            EXIT_USAGE
        })
}

/// Maps project discovery failures to the CLI's usage/failure distinction.
fn discovery_exit(error: &DiscoveryError) -> i32 {
    match error {
        DiscoveryError::NotPackageDirectory { .. }
        | DiscoveryError::MissingEntrypoint { .. }
        | DiscoveryError::NoLibrarySources { .. } => EXIT_USAGE,
        DiscoveryError::Unreadable { .. }
        | DiscoveryError::Malformed { .. }
        | DiscoveryError::LegacyMalformed { .. } => EXIT_FAILURE,
    }
}
