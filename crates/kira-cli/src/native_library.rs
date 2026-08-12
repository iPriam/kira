//! `kira build --backend llvm` inside a `kind = .Library` package.
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

use std::path::Path;

use kira_build::{Compiled, NativeLibraryArtifacts, NativeLibraryError, NativeLibraryOptions};

use crate::library::LibraryError;

/// Builds `compiled` as a native-engine library into its package's build directory.
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
        build_directory: kira_project::build_directory(source),
        toolchain_root: kira_build::toolchain_root(),
        runtime_archive: crate::native::runtime_archive(&compiled.ir)?,
        emit_llvm_ir,
    };
    Ok(kira_build::build_native_library(&compiled.ir, &options)?)
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
    fn a_package_with_no_name_is_refused_with_the_vm_engine_s_words() {
        // The refusal is shared rather than restated: the same missing manifest
        // field must read the same whichever engine the author asked for.
        let error = NativeLibraryBuildError::Package(LibraryError::Unnamed {
            path: "thing.kira".to_owned(),
        });
        assert!(error.to_string().contains("it is not inside a package"));
    }
}
