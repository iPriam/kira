//! `kira build` inside a `kind = .Library` package, on the VM engine.
//!
//! The artifact of a VM-engine library is not one file: it is a `.kbc` plus the
//! Rust crate that embeds and calls it. Both come out of
//! [`kira_build::build_library`]; this module is the part that only `kira`
//! knows — where the build directory is, what the library is called, and what to
//! print about it.
//!
//! # Why the package name and not the file stem
//!
//! Every other artifact `kira` writes is named after the source file, because
//! for a program the file *is* the thing being built. A library is not: its
//! consumer writes `uifoundation = { path = ... }` and `use uifoundation::…`,
//! and that name has to be the package's, whatever the author called the file
//! that happened to hold the exports. A package with no name to give is refused
//! rather than defaulted, because a crate silently named `main` is worse than a
//! build that says what it needs.

use std::path::{Path, PathBuf};

use kira_build::{Compiled, LibraryArtifacts, LibraryBuildError, LibraryBuildOptions};

/// Why a library build could not be started or finished.
#[derive(Debug, thiserror::Error)]
pub enum LibraryError {
    /// The package the source belongs to has no name to give the artifact.
    #[error(
        "cannot build a library from `{path}`: it is not inside a package, so there is no \
         name for the artifact or the crate that wraps it\n\
         note: add a `package.kira` declaring `Package <name> {{ let kind = .Library }}`"
    )]
    Unnamed {
        /// The source file that was handed to `kira`.
        path: String,
    },
    /// The package names itself but declares no version.
    #[error(
        "cannot build a library from `{path}`: the package `{name}` declares no version, and \
         the generated crate's `Cargo.toml` needs one\n\
         note: add `let version = \"0.1.0\"` to the package declaration"
    )]
    Unversioned {
        /// The source file that was handed to `kira`.
        path: String,
        /// The package that gave a name but no version.
        name: String,
    },
    /// The build itself failed.
    #[error(transparent)]
    Build(#[from] LibraryBuildError),
}

/// Builds `compiled` as a VM-engine library beside its source.
pub fn build(compiled: &Compiled, source: &Path) -> Result<LibraryArtifacts, LibraryError> {
    // Name and version are taken together, because a library needs both and a
    // default for either would be a wrong answer rather than a missing one: a
    // crate silently called `main`, or one silently stamped `0.0.0`, is worse
    // than a build that says what the manifest still owes.
    let Some(name) = compiled.package_name.clone() else {
        return Err(LibraryError::Unnamed {
            path: source.display().to_string(),
        });
    };
    let Some(version) = compiled.package_version.clone() else {
        return Err(LibraryError::Unversioned {
            path: source.display().to_string(),
            name,
        });
    };
    let options = LibraryBuildOptions {
        name,
        version,
        build_directory: build_directory(source),
        toolchain_root: kira_build::toolchain_root(),
    };
    Ok(kira_build::build_library(&compiled.ir, &options)?)
}

/// The `.kira-build` directory beside `source`.
///
/// The same layout every other backend writes into, so one package has one
/// build directory whatever it was built for.
fn build_directory(source: &Path) -> PathBuf {
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".kira-build")
}

/// Reports what a library build produced, in the order a consumer needs it.
pub fn report(artifacts: &LibraryArtifacts) {
    println!("Successfully built {}", artifacts.bytecode.display());
    println!(
        "  {} exports -> {}",
        artifacts.exports,
        artifacts.wrapper_crate.display()
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_directory_sits_beside_the_source() {
        let directory = build_directory(Path::new("/pkg/src/uifoundation.kira"));
        assert_eq!(directory, Path::new("/pkg/src/.kira-build"));
    }

    #[test]
    fn a_bare_file_with_no_package_is_refused_by_name() {
        let error = LibraryError::Unnamed {
            path: "thing.kira".to_owned(),
        };
        assert!(error.to_string().contains("it is not inside a package"));
    }

    #[test]
    fn a_package_with_no_version_is_refused_rather_than_stamped_with_one() {
        let error = LibraryError::Unversioned {
            path: "uifoundation.kira".to_owned(),
            name: "uifoundation".to_owned(),
        };
        let message = error.to_string();
        assert!(message.contains("declares no version"), "{message}");
        assert!(message.contains("uifoundation"), "{message}");
    }
}
