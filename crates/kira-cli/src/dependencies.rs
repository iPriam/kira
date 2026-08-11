//! Dependency declaration editing for `kira add` and `kira remove`.
//!
//! Edits go through the manifest model and the canonical declaration writer,
//! so a mutation gets the same validation, escaping, and stable formatting as
//! a newly generated package. The lockfile is intentionally not changed here;
//! `kira sync` resolves the edited graph and records the new pins.

use std::path::{Path, PathBuf};

use kira_manifest::{
    DependencySource, DependencySpec, GitSource, PathSource, ProjectManifest, RegistrySource,
    load_declaration, write_declaration,
};

use crate::pipeline::{EXIT_FAILURE, EXIT_OK, EXIT_USAGE};
use crate::progress::{err, out};

/// Runs `kira add <name> --path <path>|--version <version>|--git <url> [dir]`.
pub fn add(args: &[String]) -> i32 {
    let options = match parse_add(args) {
        Ok(options) => options,
        Err(message) => {
            err!("kira add: {message}");
            return EXIT_USAGE;
        }
    };
    let (path, mut manifest) = match read_manifest(&options.root) {
        Ok(value) => value,
        Err(message) => {
            err!("kira add: {message}");
            return EXIT_FAILURE;
        }
    };
    if let Err(error) = manifest.add_dependency(options.dependency.clone()) {
        err!("kira add: {error}");
        return EXIT_USAGE;
    }
    if let Err(error) = write_declaration(&path, &manifest) {
        err!("kira add: {error}");
        return EXIT_FAILURE;
    }
    out!(
        "added dependency `{}` to {}",
        options.dependency.name,
        path.display()
    );
    EXIT_OK
}

/// Runs `kira remove <name> [dir]`.
pub fn remove(args: &[String]) -> i32 {
    let (root, name) = match parse_remove(args) {
        Ok(value) => value,
        Err(message) => {
            err!("kira remove: {message}");
            return EXIT_USAGE;
        }
    };
    let (path, mut manifest) = match read_manifest(&root) {
        Ok(value) => value,
        Err(message) => {
            err!("kira remove: {message}");
            return EXIT_FAILURE;
        }
    };
    if manifest.remove_dependency(&name).is_none() {
        err!("kira remove: dependency `{name}` is not declared");
        return EXIT_USAGE;
    }
    if let Err(error) = write_declaration(&path, &manifest) {
        err!("kira remove: {error}");
        return EXIT_FAILURE;
    }
    out!("removed dependency `{name}` from {}", path.display());
    EXIT_OK
}

#[derive(Debug)]
struct AddOptions {
    root: PathBuf,
    dependency: DependencySpec,
}

fn parse_add(args: &[String]) -> Result<AddOptions, String> {
    let mut name = None;
    let mut root = None;
    let mut source = None;
    let mut rev = None;
    let mut tag = None;
    let mut index = 0;
    while index < args.len() {
        let argument = &args[index];
        match argument.as_str() {
            "--path" | "--version" | "--git" | "--rev" | "--tag" => {
                let value = args
                    .get(index + 1)
                    .filter(|value| !value.starts_with('-'))
                    .ok_or_else(|| format!("{argument} expects a value"))?
                    .clone();
                index += 1;
                match argument.as_str() {
                    "--path" => set_source(&mut source, SourceKind::Path(value))?,
                    "--version" => set_source(&mut source, SourceKind::Registry(value))?,
                    "--git" => set_source(&mut source, SourceKind::Git(value))?,
                    "--rev" => {
                        if rev.replace(value).is_some() {
                            return Err("--rev may be supplied only once".to_owned());
                        }
                    }
                    "--tag" => {
                        if tag.replace(value).is_some() {
                            return Err("--tag may be supplied only once".to_owned());
                        }
                    }
                    _ => unreachable!(),
                }
            }
            flag if flag.starts_with('-') => return Err(format!("unknown option `{flag}`")),
            value if name.is_none() => name = Some(value.to_owned()),
            value if root.is_none() => root = Some(PathBuf::from(value)),
            _ => return Err("expected one dependency name and one package directory".to_owned()),
        }
        index += 1;
    }

    let name = name.ok_or_else(|| "expected a dependency name".to_owned())?;
    let source = source.ok_or_else(|| {
        "expected one source: `--path <dir>`, `--version <version>`, or `--git <url>`".to_owned()
    })?;
    let dependency_source = match source {
        SourceKind::Path(path) => {
            if rev.is_some() || tag.is_some() {
                return Err("--rev and --tag require `--git`".to_owned());
            }
            DependencySource::Path(PathSource { path })
        }
        SourceKind::Registry(version) => {
            if rev.is_some() || tag.is_some() {
                return Err("--rev and --tag require `--git`".to_owned());
            }
            DependencySource::Registry(RegistrySource { version })
        }
        SourceKind::Git(url) => DependencySource::Git(GitSource { url, rev, tag }),
    };
    Ok(AddOptions {
        root: root.unwrap_or_else(|| PathBuf::from(".")),
        dependency: DependencySpec {
            name,
            source: dependency_source,
        },
    })
}

fn parse_remove(args: &[String]) -> Result<(PathBuf, String), String> {
    let mut name = None;
    let mut root = None;
    for argument in args {
        if argument.starts_with('-') {
            return Err(format!("unknown option `{argument}`"));
        }
        if name.is_none() {
            name = Some(argument.clone());
        } else if root.is_none() {
            root = Some(PathBuf::from(argument));
        } else {
            return Err("expected one dependency name and one package directory".to_owned());
        }
    }
    Ok((
        root.unwrap_or_else(|| PathBuf::from(".")),
        name.ok_or_else(|| "expected a dependency name".to_owned())?,
    ))
}

#[derive(Debug)]
enum SourceKind {
    Path(String),
    Registry(String),
    Git(String),
}

fn set_source(source: &mut Option<SourceKind>, value: SourceKind) -> Result<(), String> {
    if source.replace(value).is_some() {
        return Err("only one of `--path`, `--version`, or `--git` may be supplied".to_owned());
    }
    Ok(())
}

fn read_manifest(root: &Path) -> Result<(PathBuf, ProjectManifest), String> {
    let path = root.join("package.kira");
    let text = std::fs::read_to_string(&path)
        .map_err(|error| format!("could not read `{}`: {error}", path.display()))?;
    let manifest = load_declaration(&text)
        .map_err(|error| format!("manifest `{}` is invalid: {error}", path.display()))?;
    Ok((path, manifest))
}
