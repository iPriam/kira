//! `kirac build --backend hybrid` inside a `kind = .Library` package.
//!
//! The third engine's counterpart of [`crate::library`] and
//! [`crate::native_library`], and deliberately their mirror image: same package
//! name, same `.kira-build` layout, same generated crate directory, different
//! engine underneath.
//!
//! What differs is the artifact, and here it is three of them rather than one —
//! a bytecode half, a native half, and the manifest describing the split. Two
//! are embedded in the generated crate; the third is a shared library the
//! consumer's process opens at run time, which is why this is the only engine
//! whose report tells the author where that file is and that it travels with
//! their program.

use std::path::{Path, PathBuf};

use kira_build::{Compiled, HybridLibraryArtifacts, HybridLibraryError, HybridLibraryOptions};

use crate::library::LibraryError;

/// Builds `compiled` as a hybrid-engine library beside its source.
///
/// The name and version refusals are [`crate::library`]'s, reused rather than
/// restated: a library needs both on every engine.
pub fn build(
    compiled: &Compiled,
    source: &Path,
    emit_llvm_ir: bool,
) -> Result<HybridLibraryArtifacts, HybridLibraryBuildError> {
    let Some(name) = compiled.package_name.clone() else {
        return Err(HybridLibraryBuildError::Package(LibraryError::Unnamed {
            path: source.display().to_string(),
        }));
    };
    let Some(version) = compiled.package_version.clone() else {
        return Err(HybridLibraryBuildError::Package(
            LibraryError::Unversioned {
                path: source.display().to_string(),
                name,
            },
        ));
    };

    let options = HybridLibraryOptions {
        name,
        version,
        build_directory: build_directory(source),
        toolchain_root: kira_build::toolchain_root(),
        runtime_archive: crate::native::runtime_archive()?,
        emit_llvm_ir,
    };
    Ok(kira_build::build_hybrid_library(&compiled.ir, &options)?)
}

/// The `.kira-build` directory beside `source`.
fn build_directory(source: &Path) -> PathBuf {
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".kira-build")
}

/// Reports what a hybrid library build produced.
///
/// Says two things the other two engines' reports do not, because they are the
/// two things that are only true here: how much of the library actually became
/// machine code — a library with none of it is a VM-engine library that also
/// needs a file deployed beside it — and where that file is, since it is the one
/// artifact that does not travel inside the consumer's binary.
pub fn report(artifacts: &HybridLibraryArtifacts) {
    println!("Successfully built {}", artifacts.bytecode.display());
    println!(
        "  {} exports -> {}",
        artifacts.exports,
        artifacts.wrapper_crate.display()
    );
    println!(
        "  {} functions compiled native -> {}",
        artifacts.native_functions,
        artifacts.native_half.display()
    );
    println!("  note: the native half above ships beside a consumer's executable");
}

/// Why a hybrid library build could not be started or finished.
#[derive(Debug, thiserror::Error)]
pub enum HybridLibraryBuildError {
    /// The package does not say enough about itself to be built.
    #[error(transparent)]
    Package(#[from] LibraryError),
    /// The native runtime archive could not be located.
    #[error(transparent)]
    Runtime(#[from] crate::native::NativeError),
    /// The build itself failed.
    #[error(transparent)]
    Build(#[from] HybridLibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_directory_is_the_one_every_other_engine_writes_into() {
        assert_eq!(
            build_directory(Path::new("/pkg/src/uifoundation.kira")),
            Path::new("/pkg/src/.kira-build")
        );
    }

    #[test]
    fn a_package_with_no_version_is_refused_with_the_other_engines_words() {
        // Shared rather than restated: the same missing manifest field reads the
        // same whichever of the three engines the author asked for.
        let error = HybridLibraryBuildError::Package(LibraryError::Unversioned {
            path: "uifoundation.kira".to_owned(),
            name: "uifoundation".to_owned(),
        });
        assert!(error.to_string().contains("declares no version"), "{error}");
    }
}
