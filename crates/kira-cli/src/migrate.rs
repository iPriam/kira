//! The `kira migrate-manifest` command.
//!
//! Legacy TOML is parsed into the shared model and rendered as the canonical
//! `package.kira` declaration. The source TOML stays in place so migration is
//! recoverable and a package can compare the two files before removing the old
//! one.

use std::path::{Path, PathBuf};

use kira_manifest::{load_legacy_manifest, write_declaration};

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

const LEGACY_NAMES: [&str; 3] = ["kira.toml", "project.toml", "Kira.toml"];

/// Runs `kira migrate-manifest [dir]`.
pub fn migrate(args: &[String]) -> i32 {
    let root = match parse(args) {
        Ok(root) => root,
        Err(message) => {
            err!("kira migrate-manifest: {message}");
            return EXIT_USAGE;
        }
    };
    let destination = root.join("package.kira");
    if destination.is_file() {
        err!(
            "kira migrate-manifest: `{}` already exists; refusing to overwrite it",
            destination.display()
        );
        return EXIT_USAGE;
    }
    let Some(source) = LEGACY_NAMES
        .iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file())
    else {
        err!(
            "kira migrate-manifest: no legacy manifest found under `{}`",
            root.display()
        );
        return EXIT_USAGE;
    };
    let text = match std::fs::read_to_string(&source) {
        Ok(text) => text,
        Err(error) => {
            err!(
                "kira migrate-manifest: cannot read `{}`: {error}",
                source.display()
            );
            return EXIT_FAILURE;
        }
    };
    let manifest = match load_legacy_manifest(&text) {
        Ok(manifest) => manifest,
        Err(error) => {
            err!(
                "kira migrate-manifest: `{}` is invalid: {error}",
                source.display()
            );
            return EXIT_FAILURE;
        }
    };
    if let Err(error) = write_declaration(&destination, &manifest) {
        err!("kira migrate-manifest: {error}");
        return EXIT_FAILURE;
    }
    out!("migrated {} to {}", source.display(), destination.display());
    EXIT_OK
}

fn parse(args: &[String]) -> Result<PathBuf, String> {
    let mut root = None;
    for argument in args {
        if argument.starts_with('-') {
            return Err(format!("unknown option `{argument}`"));
        }
        if root.replace(PathBuf::from(argument)).is_some() {
            return Err("expected one package directory".to_owned());
        }
    }
    Ok(root.unwrap_or_else(|| Path::new(".").to_path_buf()))
}
