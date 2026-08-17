//! Where a build's output files go, and what each of them is called.
//!
//! Every backend's paths are decided here, Web included — one program has one
//! build directory, whatever it was built for. Splitting the naming out from the
//! building keeps the two questions apart: this module answers "where does the
//! object for this program go", and nothing in it knows what an object contains.

use std::path::{Path, PathBuf};

use kira_llvm_backend::NativeBuildTarget;

/// Where a program's build artifacts live: its package's `.kira-build/`
/// ([`kira_project::build_directory`]).
///
/// Artifacts stay inside the package they came from rather than in a shared
/// location, so two *programs* can never race for one output path. Two builds
/// of the **same** program still can — the names are the program's, not the
/// builder's — which is what the lock below is for: holding it for the life of
/// this value makes a second builder wait rather than relink an executable the
/// first one is still writing.
pub struct Artifacts {
    /// The `.kira-build` directory itself.
    directory: PathBuf,
    /// The source file's stem, which every artifact is named after.
    stem: String,
    /// Held for as long as these artifacts are being written.
    ///
    /// Never read. It exists to be dropped at the end of the build, which is
    /// when the directory becomes another builder's to use.
    _lock: crate::build_lock::BuildLock,
}

impl Artifacts {
    /// Resolves the artifact layout for `source`, creating the directory.
    pub fn for_source(source: &Path) -> Result<Self, std::io::Error> {
        Self::for_source_targeting(source, &NativeBuildTarget::host())
    }

    /// Resolves the artifact layout for a build of `source` aimed at `target`.
    ///
    /// A cross build writes into a directory named after the target, the way
    /// cargo puts a cross build under `target/<triple>/`. Both builds of one
    /// program produce an object and an executable with the same *name*, so
    /// sharing a directory means the second build silently replaces the first —
    /// and a `.kira-build/app` that is sometimes this machine's binary and
    /// sometimes an aarch64 one is not something a person or a script can
    /// reason about. It also means the two never contend for the lock, since
    /// the lock is the directory's.
    pub fn for_source_targeting(
        source: &Path,
        target: &NativeBuildTarget,
    ) -> Result<Self, std::io::Error> {
        let mut directory = kira_project::build_directory(source);
        if let Some(cross) = target.target().cross() {
            directory = directory.join(cross.normalized_triple());
        }
        // Creates the directory as well as locking it, so a caller never has
        // one without the other.
        let lock = crate::build_lock::BuildLock::acquire(&directory)?;
        let stem = source
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "program".to_owned());
        Ok(Artifacts {
            directory,
            stem,
            _lock: lock,
        })
    }

    /// The object file path.
    pub fn object(&self) -> PathBuf {
        self.directory.join(format!("{}.o", self.stem))
    }

    /// The native executable path for a build of this machine.
    pub fn executable(&self) -> PathBuf {
        self.executable_for(&NativeBuildTarget::host())
    }

    /// The native executable path for a build aimed at `target`.
    ///
    /// The extension is the *target*'s, so the file carries the one that
    /// machine needs to run it: Windows will not execute a PE by a name with no
    /// `.exe`, and this wrote the bare stem on every platform. The release gate
    /// that builds a program and runs the result is what surfaced it — the
    /// binary was there, under a name nothing could launch. A cross build has
    /// the same problem in reverse: an aarch64 Linux program written as
    /// `app.exe` because the compiler happened to run on Windows is a file
    /// nothing on the target will treat as a program either.
    pub fn executable_for(&self, target: &NativeBuildTarget) -> PathBuf {
        if target.is_windows() {
            return self.directory.join(format!("{}.exe", self.stem));
        }
        self.directory.join(&self.stem)
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

    /// The directory holding this source's finished artifacts.
    pub fn directory(&self) -> &Path {
        &self.directory
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

    /// The whole-program native library path used by an LLVM live session.
    pub fn live_library(&self) -> PathBuf {
        self.directory
            .join(shared_library_name(&format!("{}_live", self.stem)))
    }

    /// The cache marker for a VM live session's reusable native adapter surface.
    pub fn native_surface_key(&self) -> PathBuf {
        self.directory.join(format!("{}.native-surface", self.stem))
    }

    /// The shared carrier for static foreign archives.
    pub fn ffi_carrier(&self) -> PathBuf {
        self.directory
            .join(shared_library_name(&format!("{}_ffi_carrier", self.stem)))
    }

    /// The import-ordered direct Libffi binding manifest used by a live VM.
    pub fn foreign_bindings(&self) -> PathBuf {
        self.directory.join(format!("{}.ffi-bindings", self.stem))
    }
}

/// The platform's file name for a shared library called `stem`.
///
/// This host's, and deliberately so: every shared library Kira names here is one
/// *this* process loads — a hybrid half, a live library, an FFI carrier — so
/// there is no target to follow.
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

#[cfg(test)]
mod tests {
    use super::*;
    use kira_backend_api::{CrossTarget, NativeTarget, RelocationModel};
    use kira_native_lib_definition::TargetTriple;

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

    /// A cross build writes under a directory of its own, so a host build of the
    /// same program is not silently replaced by one nothing here can run.
    #[test]
    fn a_cross_build_writes_beside_the_host_build_rather_than_over_it() {
        let directory = std::env::temp_dir().join(format!(
            "kira-cross-artifacts-{}-layout",
            std::process::id()
        ));
        let source = directory.join("app.kira");
        std::fs::create_dir_all(&directory).expect("temp dir");

        let target = NativeBuildTarget::new(
            NativeTarget::Cross(CrossTarget::new(
                TargetTriple::parse("aarch64-linux-gnu").expect("a valid triple"),
                RelocationModel::Pic,
            )),
            None,
        );
        let artifacts =
            Artifacts::for_source_targeting(&source, &target).expect("cross artifact layout");
        let object = artifacts.object().display().to_string().replace('\\', "/");
        assert!(
            object.ends_with(".kira-build/aarch64-unknown-linux-gnu/app.o"),
            "{object}",
        );
        // No `.exe` on a Linux target, whatever this host would have written.
        let executable = artifacts
            .executable_for(&target)
            .display()
            .to_string()
            .replace('\\', "/");
        assert!(
            executable.ends_with(".kira-build/aarch64-unknown-linux-gnu/app"),
            "{executable}",
        );

        std::fs::remove_dir_all(&directory).ok();
    }
}
