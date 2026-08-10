//! Compiling the generated C shim translation unit with the managed clang.
//!
//! The unit itself comes from [`crate::shim`]; this is the step that turns it
//! into an object file the link can name. It sits beside [`crate::link`] rather
//! than inside it because a shim is compiled once per build and then appears on
//! four different link lines — the executable, the VM adapter sidecar, the
//! hybrid native half, and the `emcc` web link — and each of those already
//! takes an object list.
//!
//! The compiler is always the managed clang, never whatever is on `PATH`. That
//! is the same explicit-toolchain rule the rest of the backend follows, and here
//! it carries extra weight: the whole design delegates the by-value ABI decision
//! to this compiler, so which compiler it is has to be a fact rather than an
//! accident of the machine.

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_runtime_abi::{ForeignAggregates, ForeignCallback, ForeignImport};
use kira_toolchain::LlvmInstallation;

use crate::LlvmError;
use crate::shim;

/// The compiled shim object a build links, when the program needs one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShimObject {
    /// The generated C source, kept beside the object.
    ///
    /// Written out rather than piped through stdin so a failing link or a
    /// surprising ABI can be investigated by reading the exact C that was
    /// compiled.
    pub source: PathBuf,
    /// The object file to put on the link line.
    pub object: PathBuf,
}

/// Compiles the foreign shim for `imports` and `callbacks` beside
/// `object_path`, if one is needed.
///
/// Returns `None` when no position in either direction names an aggregate — the
/// common case, which never invokes clang at all. The artifacts are named after
/// `object_path`'s stem so two builds in one directory do not collide.
///
/// Takes the seam rows rather than the whole program: what a shim needs is
/// exactly the seam, and a narrower argument is one a test can build.
pub fn build(
    imports: &[ForeignImport],
    callbacks: &[ForeignCallback],
    table: &ForeignAggregates,
    unavailable: &[usize],
    object_path: &Path,
    llvm: &LlvmInstallation,
) -> Result<Option<ShimObject>, LlvmError> {
    let Some(text) = shim::generate(imports, callbacks, table, unavailable) else {
        return Ok(None);
    };

    let stem = object_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("kira");
    let directory = object_path.parent().unwrap_or_else(|| Path::new("."));
    let source = directory.join(format!("{stem}_ffi_shim.c"));
    let object = directory.join(format!("{stem}_ffi_shim.o"));

    if let Some(parent) = source.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LlvmError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    std::fs::write(&source, &text).map_err(|error| LlvmError::Io {
        path: source.clone(),
        source: error,
    })?;

    let driver = llvm.clang();
    if !driver.is_file() {
        return Err(LlvmError::Link(crate::LinkError::DriverMissing {
            path: driver,
        }));
    }
    let output = Command::new(&driver)
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .map_err(|error| LlvmError::Io {
            path: driver.clone(),
            source: error,
        })?;
    if !output.status.success() {
        return Err(LlvmError::ShimUncompilable {
            source_path: source,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }

    Ok(Some(ShimObject { source, object }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compiling a program with no aggregate never reaches the compiler.
    ///
    /// Worth pinning: this is what keeps every existing FFI build — and every
    /// build with no FFI at all — from gaining a clang subprocess it does not
    /// need.
    #[test]
    fn a_program_with_no_aggregate_builds_no_shim() {
        use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType};

        let llvm = kira_toolchain::discover(None).expect("the managed LLVM is present");
        let scalar_only = [ForeignImport::new(
            "fixture",
            "plain",
            ForeignAbi::C,
            ForeignSignature::scalars([ForeignType::I32], ForeignType::I32),
        )];
        for imports in [&scalar_only[..], &[][..]] {
            let built = build(
                imports,
                &[],
                &ForeignAggregates::new(),
                &[],
                Path::new("/tmp/kira-shim-none.o"),
                &llvm,
            )
            .expect("no aggregate is not an error");
            assert_eq!(built, None, "no clang subprocess and no file written");
        }
        assert!(
            !Path::new("/tmp/kira-shim-none_ffi_shim.c").exists(),
            "nothing is written for a program that needs no shim",
        );
    }
}
