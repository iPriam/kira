//! The `kira new` project scaffolder.

use std::path::{Path, PathBuf};

use kira_app_generation::TemplateKind;

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Generates a new app or library project in an empty directory.
pub fn new(args: &[String]) -> i32 {
    let (kind, root) = match parse(args) {
        Ok(options) => options,
        Err(message) => {
            err!("kira new: {message}");
            return EXIT_USAGE;
        }
    };
    let package_name = package_name(&root);
    match kind.generate(&root, &package_name) {
        Ok(generated) => {
            out!(
                "created {} project `{}` at {}",
                kind.label(),
                package_name,
                root.display()
            );
            for file in generated.files {
                out!("  {}", file.display());
            }
            EXIT_OK
        }
        Err(error) => {
            err!("kira new: {error}");
            EXIT_FAILURE
        }
    }
}

/// The command's small option set.
fn parse(args: &[String]) -> Result<(TemplateKind, PathBuf), String> {
    let mut kind = TemplateKind::App;
    let mut root = None;
    for argument in args {
        match argument.as_str() {
            "--app" => kind = TemplateKind::App,
            "--library" => kind = TemplateKind::Library,
            flag if flag.starts_with('-') => {
                return Err(format!("unknown option `{flag}`"));
            }
            path if root.replace(PathBuf::from(path)).is_some() => {
                return Err("expected one destination directory".to_owned());
            }
            path => root = Some(PathBuf::from(path)),
        }
    }
    root.map(|path| (kind, path))
        .ok_or_else(|| "expected a destination directory".to_owned())
}

/// Derives a package identifier from the destination directory name.
fn package_name(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty() && *name != ".")
        .map_or_else(|| "App".to_owned(), ToOwned::to_owned)
}
