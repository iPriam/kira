//! Building a Kira library into what a Rust consumer depends on.
//!
//! Two artifacts, and the second is the point:
//!
//! ```text
//! .kira-build/lib/uifoundation.kbc        the compiled library
//! .kira-build/rust/uifoundation/          the crate a Rust program depends on
//!     Cargo.toml
//!     README.md
//!     uifoundation.kbc                    the same bytes, embedded
//!     src/lib.rs
//! ```
//!
//! # Why the bytecode is written twice
//!
//! `.kira-build/lib/` is where the library artifact lives, and it is what the
//! other two engines write their own artifacts beside. The copy inside the
//! generated crate is what `include_bytes!` reads, and it lives *in* the crate
//! so the crate is relocatable: copy the directory anywhere — into a container,
//! into another checkout — and it still finds its bytes. A generated crate that
//! reached back out to an absolute path would work exactly until someone moved
//! it, and then fail at build time with a path nobody wrote.
//!
//! The duplication costs a few kilobytes and buys the property outright, which
//! is the trade every embedded-artifact scheme makes.

use std::path::{Path, PathBuf};

use kira_ir::IrProgram;

use crate::wrapper::{self, WrapperSpec};

/// What a library build needs to know beyond the program itself.
#[derive(Debug, Clone)]
pub struct LibraryBuildOptions {
    /// The library's package name: the artifact's name, and the crate's.
    pub name: String,
    /// The library's version, from its manifest.
    pub version: String,
    /// The `.kira-build` directory to write under.
    pub build_directory: PathBuf,
    /// The Kira checkout the generated crate takes its path dependencies from.
    pub toolchain_root: PathBuf,
}

/// What a library build produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryArtifacts {
    /// The compiled library.
    pub bytecode: PathBuf,
    /// The root of the generated wrapper crate.
    pub wrapper_crate: PathBuf,
    /// The hash the generated wrapper checks the artifact against.
    pub content_hash: u64,
    /// How many exports the wrapper offers a method for.
    pub exports: usize,
}

/// Why a library could not be built.
#[derive(Debug, thiserror::Error)]
pub enum LibraryBuildError {
    /// The program did not compile to bytecode.
    #[error("bytecode compilation failed: {0}")]
    Compile(#[from] kira_bytecode::CompileError),
    /// The export surface has no legal Rust spelling.
    #[error("this library's export surface cannot be generated for Rust: {0}")]
    Wrapper(#[from] wrapper::WrapperError),
    /// An artifact could not be written.
    #[error("cannot write `{path}`: {source}")]
    Write {
        /// The path that could not be written.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// Compiles `program` as a library and generates the crate that consumes it.
pub fn build_library(
    program: &IrProgram,
    options: &LibraryBuildOptions,
) -> Result<LibraryArtifacts, LibraryBuildError> {
    let module = kira_bytecode::compile(program)?;
    let bytes = module.to_bytes();
    let content_hash = kira_main::content_hash(&bytes);

    let artifact_name = wrapper::artifact_file_name(&options.name);
    let bytecode = options.build_directory.join("lib").join(&artifact_name);
    write(&bytecode, &bytes)?;

    let generated = wrapper::generate(&WrapperSpec {
        library: &options.name,
        version: &options.version,
        exports: &module.exports,
        content_hash,
        toolchain_root: &options.toolchain_root,
    })?;

    let wrapper_crate = options.build_directory.join("rust").join(&generated.name);
    for file in &generated.files {
        write(&wrapper_crate.join(&file.path), file.contents.as_bytes())?;
    }
    // The embedded copy: `include_bytes!("../<name>.kbc")` from `src/lib.rs`.
    write(&wrapper_crate.join(&artifact_name), &bytes)?;
    remove_foreign_engine_files(&wrapper_crate, &options.name)?;

    Ok(LibraryArtifacts {
        bytecode,
        wrapper_crate,
        content_hash,
        exports: module.exports.functions.len(),
    })
}

/// Removes what the native engine left in this crate directory.
///
/// Building the same package for the other engine writes into the same
/// directory, and the native engine's `build.rs` would otherwise survive the
/// switch — cargo runs a build script it *finds*, so the VM-engine crate would
/// go on linking a stale archive. `build = false` in the generated manifest says
/// the same thing a second way; this is the half that also stops the file being
/// read by anything else looking at the directory.
fn remove_foreign_engine_files(
    wrapper_crate: &Path,
    library: &str,
) -> Result<(), LibraryBuildError> {
    for file in wrapper::foreign_engine_files(wrapper::Engine::Vm, library) {
        let path = wrapper_crate.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(LibraryBuildError::Write {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Writes `contents` to `path`, creating the directories above it.
fn write(path: &Path, contents: &[u8]) -> Result<(), LibraryBuildError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| LibraryBuildError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| LibraryBuildError::Write {
        path: path.display().to_string(),
        source,
    })
}

/// The Kira checkout this build of the compiler came from.
///
/// The generated crate depends on `kira-main` and the crates under it by path,
/// because none of them is published yet, and the only path that is true is the
/// one this compiler was built from. Baked in at compile time rather than
/// discovered at run time: a discovered root would depend on where `kirac`
/// happens to be invoked, and would silently generate a crate pointing at
/// somebody else's checkout.
///
/// This is v1's answer and it is stated as such in the generated README. When
/// the runtime crates publish, this becomes a version requirement and the
/// generated crate becomes something you could commit.
pub fn toolchain_root() -> PathBuf {
    // `<root>/crates/kira-build` — two levels up is the checkout.
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or(manifest)
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory that removes itself.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            let base = std::env::temp_dir().join(format!(
                "kira-build-library-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id(),
            ));
            std::fs::create_dir_all(&base).expect("a scratch directory");
            TempDir(base)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Compiles a library source through the real frontend.
    fn library_ir(dir: &TempDir, source: &str) -> IrProgram {
        std::fs::write(
            dir.0.join("package.kira"),
            "Package uifoundation {\n    let version = \"0.1.0\"\n    let kind = .Library\n}\n",
        )
        .expect("write package.kira");
        let path = dir.0.join("uifoundation.kira");
        std::fs::write(&path, source).expect("write source");
        let compiled = crate::frontend::compile(&path).expect("compile");
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        compiled.ir
    }

    fn options(dir: &TempDir) -> LibraryBuildOptions {
        LibraryBuildOptions {
            name: "uifoundation".to_owned(),
            version: "0.1.0".to_owned(),
            build_directory: dir.0.join(".kira-build"),
            toolchain_root: toolchain_root(),
        }
    }

    #[test]
    fn a_library_build_writes_the_artifact_and_the_crate_around_it() {
        let dir = TempDir::new("artifacts");
        let ir = library_ir(
            &dir,
            "@Export\nclass Button {\n    var title: String = \"\"\n}\n\
             @Export\nfunction makeButton(title: String) -> Button { var b = Button() b.title = title return b }\n",
        );
        let built = build_library(&ir, &options(&dir)).expect("build");

        assert!(built.bytecode.is_file(), "{}", built.bytecode.display());
        assert!(built.bytecode.ends_with(".kira-build/lib/uifoundation.kbc"));
        assert_eq!(built.exports, 1);
        for file in ["Cargo.toml", "README.md", "src/lib.rs", "uifoundation.kbc"] {
            let path = built.wrapper_crate.join(file);
            assert!(path.is_file(), "{} is missing", path.display());
        }
    }

    #[test]
    fn the_embedded_copy_is_the_same_bytes_as_the_artifact() {
        // The generated crate checks a content hash it was given at generation
        // time. If the two copies could differ, that check would fail on a
        // library nobody had touched.
        let dir = TempDir::new("copies");
        let ir = library_ir(
            &dir,
            "@Export\nfunction add(a: Int, b: Int) -> Int { return a + b }\n",
        );
        let built = build_library(&ir, &options(&dir)).expect("build");
        let artifact = std::fs::read(&built.bytecode).expect("read artifact");
        let embedded =
            std::fs::read(built.wrapper_crate.join("uifoundation.kbc")).expect("read embedded");
        assert_eq!(artifact, embedded);
        assert_eq!(kira_main::content_hash(&artifact), built.content_hash);
    }

    #[test]
    fn the_generated_wrapper_verifies_against_the_artifact_it_was_built_from() {
        // The whole stale-build guard, exercised end to end without compiling
        // any Rust: load the artifact the way the generated `load()` does, and
        // check it against the same surface the generated `CONTRACT` names.
        let dir = TempDir::new("verify");
        let ir = library_ir(
            &dir,
            "@Export\nfunction add(a: Int, b: Int) -> Int { return a + b }\n",
        );
        let built = build_library(&ir, &options(&dir)).expect("build");
        let bytes = std::fs::read(&built.bytecode).expect("read artifact");
        let library = kira_main::Library::from_bytes(&bytes).expect("load");
        assert_eq!(library.content_hash(), built.content_hash);
        assert!(library.export("add").is_some());
    }

    #[test]
    fn a_library_that_exports_nothing_still_produces_a_crate() {
        let dir = TempDir::new("empty");
        let ir = library_ir(
            &dir,
            "function add(a: Int, b: Int) -> Int { return a + b }\n",
        );
        let built = build_library(&ir, &options(&dir)).expect("build");
        assert_eq!(built.exports, 0);
        assert!(built.wrapper_crate.join("src/lib.rs").is_file());
    }

    #[test]
    fn a_vm_build_removes_the_native_engines_build_script() {
        // The switch flow: `kirac build --backend llvm` then `kirac build`, in
        // one package. Both engines write the same directory, and cargo decides
        // whether a crate has a build script by looking for the file — so a
        // surviving `build.rs` would make the VM-engine crate go on linking the
        // native engine's stale archive, silently.
        //
        // The native half is stood in for by writing the file, because building
        // it needs LLVM and this property is not about LLVM.
        let dir = TempDir::new("switch");
        let ir = library_ir(
            &dir,
            "@Export\nfunction add(a: Int, b: Int) -> Int { return a + b }\n",
        );
        let built = build_library(&ir, &options(&dir)).expect("build");
        let script = built.wrapper_crate.join("build.rs");
        std::fs::write(&script, "fn main() { /* the native engine's */ }\n").expect("plant");

        let built = build_library(&ir, &options(&dir)).expect("rebuild");
        assert!(
            !script.exists(),
            "the native engine's build script survived a VM build: {}",
            script.display()
        );
        // And the manifest says so too, so the answer does not depend on the
        // directory having been cleaned.
        let manifest =
            std::fs::read_to_string(built.wrapper_crate.join("Cargo.toml")).expect("read manifest");
        assert!(manifest.contains("\nbuild = false\n"), "{manifest}");
    }

    #[test]
    fn the_toolchain_root_is_the_checkout_this_compiler_came_from() {
        assert!(toolchain_root().join("crates").join("kira-main").is_dir());
    }
}
