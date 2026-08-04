//! The native (LLVM) half of `build` and `run`: artifact layout, locating the
//! runtime archive, and executing a built program.
//!
//! [`Artifacts`] is where every backend's output paths are decided, Web
//! included — one program has one build directory, whatever it was built for.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;
use kira_llvm_backend::{
    AdapterSidecarOptions, LlvmError, NativeArtifacts, NativeBuildOptions, NativeLinkInputs,
};

/// Where a program's build artifacts live: `<source-dir>/.kira-build/`.
///
/// Artifacts stay beside the program they came from rather than in a shared
/// location, so two programs can never race for one output path.
pub struct Artifacts {
    /// The `.kira-build` directory itself.
    directory: PathBuf,
    /// The source file's stem, which every artifact is named after.
    stem: String,
}

impl Artifacts {
    /// Resolves the artifact layout for `source`, creating the directory.
    pub fn for_source(source: &Path) -> Result<Self, std::io::Error> {
        let directory = source
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(".kira-build");
        std::fs::create_dir_all(&directory)?;
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned());
        Ok(Artifacts { directory, stem })
    }

    /// The object file path.
    pub fn object(&self) -> PathBuf {
        self.directory.join(format!("{}.o", self.stem))
    }

    /// The native executable path.
    ///
    /// Through `executable_name`, so the file carries the extension its host
    /// needs to run it: Windows will not execute a PE by a name with no `.exe`,
    /// and this wrote the bare stem on every platform. The release gate that
    /// builds a program and runs the result is what surfaced it — the binary
    /// was there, under a name nothing could launch.
    pub fn executable(&self) -> PathBuf {
        self.directory
            .join(kira_toolchain::executable_name(&self.stem))
    }

    /// The textual LLVM IR dump path.
    pub fn llvm_ir(&self) -> PathBuf {
        self.directory.join(format!("{}.ll", self.stem))
    }

    /// The directory holding the Web artifacts, which is what `run` serves.
    ///
    /// Separate from the rest because it is exposed over HTTP: only what a
    /// browser needs belongs under a served root.
    pub fn web_directory(&self) -> PathBuf {
        self.directory.join("web")
    }

    /// The source file's stem, which every artifact is named after.
    pub fn stem(&self) -> &str {
        &self.stem
    }

    /// The hybrid manifest path: the artifact a hybrid run loads first.
    pub fn manifest(&self) -> PathBuf {
        self.directory.join(format!("{}.khm", self.stem))
    }

    /// The bytecode payload path, for a hybrid build.
    pub fn bytecode(&self) -> PathBuf {
        self.directory.join(format!("{}.kbc", self.stem))
    }

    /// The shared library path holding a hybrid program's native half.
    ///
    /// Named the way the host platform's loader expects, so `dlopen` finds it
    /// by the name the manifest records.
    pub fn shared_library(&self) -> PathBuf {
        self.directory.join(shared_library_name(&self.stem))
    }

    /// The object file for the VM's foreign-adapter sidecar.
    fn foreign_object(&self) -> PathBuf {
        self.directory.join(format!("{}_ffi.o", self.stem))
    }

    /// The VM's foreign-adapter sidecar shared library.
    ///
    /// A separate name from the hybrid dylib so a VM build and a hybrid build of
    /// the same program never write to one path.
    pub fn foreign_sidecar(&self) -> PathBuf {
        self.directory
            .join(shared_library_name(&format!("{}_ffi", self.stem)))
    }
}

/// The platform's file name for a shared library called `stem`.
fn shared_library_name(stem: &str) -> String {
    let extension = if cfg!(target_os = "macos") {
        "dylib"
    } else if cfg!(target_os = "windows") {
        "dll"
    } else {
        "so"
    };
    if cfg!(target_os = "windows") {
        format!("{stem}.{extension}")
    } else {
        format!("lib{stem}.{extension}")
    }
}

/// Builds `program` into a native executable, linking `foreign_link`.
///
/// The archives are the selected C static libraries that satisfy the program's
/// `@FFI.Extern` imports; each generated adapter references its C symbol, so
/// naming them on the link line pulls in exactly the members those symbols need.
pub fn build(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
    optimize: bool,
    foreign_link: &NativeLinkInputs,
) -> Result<NativeArtifacts, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let options = NativeBuildOptions {
        module_name: artifacts.stem.clone(),
        object_path: artifacts.object(),
        executable_path: Some(artifacts.executable()),
        // A whole-program native build has no second half to load.
        shared_library_path: None,
        // A program is entered at `main` and exports nothing.
        archive_path: None,
        exports: kira_llvm_backend::NativeExportSurface::default(),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: runtime_archive(program)?,
        optimize,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
    };
    Ok(kira_llvm_backend::build_native(program, &options)?)
}

/// Builds the VM's foreign-adapter sidecar for `program`, returning its path.
///
/// The VM never links or `dlopen`s anything itself; this is what a VM build
/// produces so a native-capable host can answer `call_foreign`. The sidecar
/// carries one exported adapter per foreign import, the foreign-adapter marker,
/// the string helpers, and the selected C archives — one self-contained file.
pub fn build_adapter_sidecar(
    program: &IrProgram,
    source: &Path,
    foreign_link: &NativeLinkInputs,
) -> Result<PathBuf, NativeError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;
    let options = AdapterSidecarOptions {
        module_name: format!("{}_ffi", artifacts.stem),
        object_path: artifacts.foreign_object(),
        library_path: artifacts.foreign_sidecar(),
        runtime_archive: runtime_archive(program)?,
        unavailable_imports: foreign_link.unavailable_imports().to_vec(),
        foreign_link: foreign_link.clone(),
    };
    Ok(kira_llvm_backend::build_adapter_sidecar(program, &options)?)
}

/// Runs a built native executable, returning its exit code.
///
/// The child inherits this process's streams, so a native run's output is
/// indistinguishable from a VM run's.
pub fn execute(executable: &Path) -> Result<i32, NativeError> {
    let status = std::process::Command::new(executable)
        .status()
        .map_err(|source| NativeError::Spawn {
            executable: executable.to_path_buf(),
            source,
        })?;
    // A signal-killed child reports no code; surface it as a failure rather
    // than as success.
    Ok(status.code().unwrap_or(1))
}

/// Locates the native runtime archive `program` needs.
///
/// Two archives, and the program picks. The base one carries the runtime every
/// native program needs; `libkira_compiler_bridge.a` carries that *and* the
/// check-only frontend, because native code has no host to ask for a compiler
/// and can only reach one that was linked in. Linking the larger one always
/// would put a compiler inside every program Kira ever produces, and linking
/// both is not possible — two Rust static libraries in one link line duplicate
/// the standard library — so the answer is whichever one this program needs.
///
/// Either sits beside this executable: cargo writes both staticlibs into the
/// same profile directory as `kira`, and `kira` depends on both crates, so a
/// built `kira` always has matching archives next to it.
pub fn runtime_archive(program: &IrProgram) -> Result<PathBuf, NativeError> {
    let executable =
        std::env::current_exe().map_err(|source| NativeError::RuntimeArchive { source })?;
    let directory = executable
        .parent()
        .ok_or_else(|| NativeError::RuntimeArchive {
            source: std::io::Error::other("this executable has no parent directory"),
        })?;
    Ok(directory.join(archive_file_name(program.uses_compiler())))
}

/// Which archive file a program needs, by name.
///
/// Split from the path so a test can assert the choice without a built `kira`
/// beside it.
///
/// The spelling is the host toolchain's, because the file being named is one
/// cargo just wrote for this host: MSVC writes `<name>.lib` and everything else
/// writes `lib<name>.a`. Naming it the Unix way on Windows looks for a file
/// cargo never produced, which is the "native runtime archive is missing" error
/// with nothing missing.
fn archive_file_name(uses_compiler: bool) -> &'static str {
    let crate_name = match uses_compiler {
        true => "kira_compiler_bridge",
        false => "kira_native_bridge",
    };
    match (crate_name, cfg!(target_env = "msvc")) {
        ("kira_compiler_bridge", true) => "kira_compiler_bridge.lib",
        ("kira_compiler_bridge", false) => "libkira_compiler_bridge.a",
        (_, true) => "kira_native_bridge.lib",
        (_, false) => "libkira_native_bridge.a",
    }
}

/// Why a native build or run failed.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    /// The artifact directory could not be prepared.
    #[error("cannot create the `.kira-build` directory: {source}")]
    Layout {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The runtime archive could not be located.
    #[error("cannot locate the native runtime archive: {source}")]
    RuntimeArchive {
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The backend failed.
    #[error(transparent)]
    Backend(#[from] LlvmError),
    /// The built executable could not be started.
    #[error("cannot run `{executable}`: {source}")]
    Spawn {
        /// The executable that could not be started.
        executable: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A program that never checks a package links the small archive; one that
    /// does links the archive that carries a compiler. Both, never neither and
    /// never both — two Rust static libraries in one link line do not link.
    #[test]
    fn the_archive_a_program_links_follows_from_whether_it_checks_packages() {
        // Asserted against the host's own spelling: the name has to be the one
        // cargo wrote next to this binary, so a test that pinned the Unix name
        // everywhere would pass on the platform where the name is wrong.
        if cfg!(target_env = "msvc") {
            assert_eq!(archive_file_name(false), "kira_native_bridge.lib");
            assert_eq!(archive_file_name(true), "kira_compiler_bridge.lib");
        } else {
            assert_eq!(archive_file_name(false), "libkira_native_bridge.a");
            assert_eq!(archive_file_name(true), "libkira_compiler_bridge.a");
        }
    }

    #[test]
    fn artifacts_live_beside_their_source_and_share_its_stem() {
        let directory = std::env::temp_dir().join("kira-artifacts-test");
        let source = directory.join("hello.kira");
        std::fs::create_dir_all(&directory).expect("temp dir");

        let artifacts = Artifacts::for_source(&source).expect("layout");
        assert!(artifacts.object().ends_with(".kira-build/hello.o"));
        assert!(artifacts.executable().ends_with(format!(
            ".kira-build/{}",
            kira_toolchain::executable_name("hello")
        )));
        assert!(artifacts.llvm_ir().ends_with(".kira-build/hello.ll"));
        assert!(artifacts.object().starts_with(&directory));

        std::fs::remove_dir_all(&directory).ok();
    }
}
