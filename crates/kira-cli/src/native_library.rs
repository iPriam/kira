//! `kirac build --backend llvm` inside a `kind = .Library` package.
//!
//! The native engine's counterpart of [`crate::library`], and deliberately its
//! mirror image: same package name, same `.kira-build` layout, same generated
//! crate directory, different engine underneath. A consumer's own code does not
//! change when the engine does, which is the property the whole feature is
//! measured on — so neither does the shape of the build that produces it.
//!
//! What differs is the artifact. The VM engine writes a `.kbc` the crate
//! embeds; this writes a static archive the crate *links*, self-contained down
//! to the Kira native runtime, so the consumer needs no LLVM and no arrangement
//! with the toolchain.

use std::path::{Path, PathBuf};

use kira_build::{Compiled, NativeLibraryArtifacts, NativeLibraryError, NativeLibraryOptions};

use crate::library::LibraryError;

/// Builds `compiled` as a native-engine library beside its source.
///
/// The name and version refusals are [`crate::library`]'s, reused rather than
/// restated: a library needs both on every engine, and two spellings of "this
/// package declares no version" would be one too many.
pub fn build(
    compiled: &Compiled,
    source: &Path,
    emit_llvm_ir: bool,
) -> Result<NativeLibraryArtifacts, NativeLibraryBuildError> {
    let Some(name) = compiled.package_name.clone() else {
        return Err(NativeLibraryBuildError::Package(LibraryError::Unnamed {
            path: source.display().to_string(),
        }));
    };
    let Some(version) = compiled.package_version.clone() else {
        return Err(NativeLibraryBuildError::Package(
            LibraryError::Unversioned {
                path: source.display().to_string(),
                name,
            },
        ));
    };

    let options = NativeLibraryOptions {
        name,
        version,
        build_directory: build_directory(source),
        toolchain_root: kira_build::toolchain_root(),
        runtime_archive: crate::native::runtime_archive()?,
        emit_llvm_ir,
    };
    Ok(kira_build::build_native_library(&compiled.ir, &options)?)
}

/// The `.kira-build` directory beside `source`.
fn build_directory(source: &Path) -> PathBuf {
    source
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(".kira-build")
}

/// Reports what a native library build produced, in the order a consumer needs.
pub fn report(artifacts: &NativeLibraryArtifacts) {
    println!("Successfully built {}", artifacts.archive.display());
    println!(
        "  {} exports -> {}",
        artifacts.exports,
        artifacts.wrapper_crate.display()
    );
}

/// Why a native library build could not be started or finished.
#[derive(Debug, thiserror::Error)]
pub enum NativeLibraryBuildError {
    /// The package does not say enough about itself to be built.
    #[error(transparent)]
    Package(#[from] LibraryError),
    /// The native runtime archive could not be located.
    #[error(transparent)]
    Runtime(#[from] crate::native::NativeError),
    /// The build itself failed.
    #[error(transparent)]
    Build(#[from] NativeLibraryError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_build_directory_is_the_one_every_other_engine_writes_into() {
        // One package has one build directory whatever it was built for, so a
        // library built twice for two engines leaves both artifacts side by
        // side rather than one on top of the other.
        assert_eq!(
            build_directory(Path::new("/pkg/src/uifoundation.kira")),
            Path::new("/pkg/src/.kira-build")
        );
    }

    #[test]
    fn a_package_with_no_name_is_refused_with_the_vm_engine_s_words() {
        // The refusal is shared rather than restated: the same missing manifest
        // field must read the same whichever engine the author asked for.
        let error = NativeLibraryBuildError::Package(LibraryError::Unnamed {
            path: "thing.kira".to_owned(),
        });
        assert!(error.to_string().contains("it is not inside a package"));
    }
}
