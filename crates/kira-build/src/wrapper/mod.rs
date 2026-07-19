//! The generated Rust wrapper crate: what it is, and how it is produced.
//!
//! A Kira library compiled for the VM engine is a `.kbc` file, and a `.kbc` file
//! is not something a Rust program can call. The wrapper crate is what closes
//! that: it `include_bytes!`s the artifact, holds one persistent
//! [`kira_main::Instance`] behind an `Rc<RefCell<_>>`, and exposes one safe
//! method per `@Export` plus one newtype per exported class.
//!
//! [`kira_main::Instance`]: https://docs.rs/kira-main
//!
//! # Pure on purpose
//!
//! [`generate`] touches no filesystem: it takes a [`WrapperSpec`] and returns a
//! [`GeneratedCrate`], which is a list of file paths and their contents.
//! Everything about the output is therefore assertable in a unit test without a
//! temp directory, and the one place that writes files ([`crate::library`]) has
//! nothing in it worth testing beyond "it wrote what it was given".
//!
//! # Generated code is code
//!
//! What comes out is held to this workspace's Rust bar, because the consumer's
//! `cargo clippy -D warnings` will hold it there whether or not this crate does:
//! a doc comment on every public item, no `unsafe`, no `unwrap`, and no
//! `#[allow]`. The one deliberate exception is a crate-level doc comment: the
//! generated `src/lib.rs` opens with plain `//` comments rather than `//!`, so
//! that a consumer may `include!` the file into a module of its own. An inner
//! doc comment cannot survive that, and being includable is what lets a
//! `build.rs` generate the wrapper into `OUT_DIR` instead of forcing every
//! consumer to carry a path dependency on a directory that does not exist until
//! something builds it. The crate-level prose lives in the generated
//! `README.md`.

mod manifest;
mod naming;
pub(crate) mod render;
mod render_native;

use std::path::{Path, PathBuf};

use kira_bytecode::ExportTable;
use kira_llvm_backend::NativeExportSurface;

/// What a wrapper crate is generated from.
///
/// Borrows its inputs: this is a description handed to a generator and consumed
/// immediately, not something anything stores.
#[derive(Debug, Clone, Copy)]
pub struct WrapperSpec<'a> {
    /// The library's package name, which is also the crate and module name.
    pub library: &'a str,
    /// The version to give the generated crate, from the library's manifest.
    pub version: &'a str,
    /// The export surface the wrapper offers methods for.
    pub exports: &'a ExportTable,
    /// [`kira_main::content_hash`] of the artifact the wrapper embeds.
    ///
    /// [`kira_main::content_hash`]: https://docs.rs/kira-main
    pub content_hash: u64,
    /// The Kira checkout the generated crate takes its path dependencies from.
    ///
    /// v1 resolves dependencies as paths into the toolchain that generated the
    /// crate, which is why the crate is regenerated rather than committed.
    /// Published crates are the eventual answer; see the generated README.
    pub toolchain_root: &'a Path,
}

/// One file of a generated crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedFile {
    /// Where it goes, relative to the crate root.
    pub path: PathBuf,
    /// What it contains.
    pub contents: String,
}

/// A whole generated crate, in memory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedCrate {
    /// The crate's name, which is also its directory name.
    pub name: String,
    /// Every text file it is made of, in a stable order.
    ///
    /// The `.kbc` artifact is *not* here: it is bytes rather than text, and the
    /// library build already has it in hand.
    pub files: Vec<GeneratedFile>,
}

impl GeneratedCrate {
    /// The contents of one file, by its crate-relative path.
    ///
    /// For tests and for anything that wants to inspect the output without
    /// writing it to disk.
    pub fn file(&self, path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.path == Path::new(path))
            .map(|file| file.contents.as_str())
    }
}

/// Why an export surface could not be turned into a Rust crate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WrapperError {
    /// A name in the library has no legal Rust spelling.
    #[error("{kind} named `{name}` cannot be exported to Rust: {reason}")]
    Unspellable {
        /// What kind of thing carries the name, for the message.
        kind: &'static str,
        /// The name as the library spells it.
        name: String,
        /// Why Rust cannot spell it.
        reason: &'static str,
    },
    /// Two things in the library map onto one Rust name.
    ///
    /// The frontend already refuses two exports that collide, so reaching this
    /// means two *classes*, or a class and the library type, landed on one name.
    #[error("`{first}` and `{second}` both become the Rust name `{rust}`")]
    Collision {
        /// The Rust name they agree on.
        rust: String,
        /// The first thing that claimed it.
        first: String,
        /// The second thing that claimed it.
        second: String,
    },
    /// A class claims the name the generated code needs for its host parameter.
    ///
    /// The library type and every handle newtype are generic over the host the
    /// embedder supplies, and that parameter has to be called something. A class
    /// of the same name would make the generated file name a type parameter
    /// where a type belongs, so it is refused here with the reason rather than
    /// in the consumer's build with a borrow-checker-shaped message.
    #[error(
        "an exported class may not be named `{name}`: the generated wrapper spells its host \
         type parameter `{name}`"
    )]
    Reserved {
        /// The Rust name the class landed on.
        name: String,
    },
    /// The backend's export surface is missing a symbol the wrapper needs.
    ///
    /// Unreachable when the surface and the export table came from the same
    /// build, which is the only way this crate produces them — and checked
    /// anyway, because emitting an `extern` block with a hole in it would be a
    /// link failure in the consumer's crate against a symbol nobody named.
    #[error("the native build produced no symbol for {what}")]
    MissingSymbol {
        /// What had no symbol, for the message.
        what: &'static str,
    },
    /// A handle type names a class the export table does not have.
    ///
    /// Unreachable through a module this compiler wrote and validated, and
    /// checked anyway: a `.kbc` is a public artifact, and a generator that
    /// trusted one would emit a Rust file naming a type that does not exist.
    #[error("a handle names exported class {class}, which this library does not have")]
    UnknownClass {
        /// The out-of-range index into the class list.
        class: u32,
    },
}

/// Generates the VM-engine wrapper crate for one library.
pub fn generate(spec: &WrapperSpec<'_>) -> Result<GeneratedCrate, WrapperError> {
    generate_for(spec, render::EngineBinding::Vm)
}

/// Generates the hybrid-engine wrapper crate for one library.
///
/// The same files, from the same renderer, with the engine binding swapped —
/// see [`render::EngineBinding`] for why that is one function rather than two.
/// `native_half` is the absolute path the build wrote the shared library to,
/// which the generated `load()` treats as the *last* place to look.
pub fn generate_hybrid(
    spec: &WrapperSpec<'_>,
    manifest_file: &str,
    native_half: &Path,
) -> Result<GeneratedCrate, WrapperError> {
    generate_for(
        spec,
        render::EngineBinding::Hybrid {
            manifest: manifest_file.to_owned(),
            native_half: native_half.display().to_string(),
        },
    )
}

/// The generator both VM-family engines share.
fn generate_for(
    spec: &WrapperSpec<'_>,
    engine: render::EngineBinding,
) -> Result<GeneratedCrate, WrapperError> {
    let model = render::Model::build(spec, engine)?;
    Ok(GeneratedCrate {
        name: model.library.clone(),
        files: vec![
            GeneratedFile {
                path: PathBuf::from("Cargo.toml"),
                contents: manifest::cargo_toml(spec, &model),
            },
            GeneratedFile {
                path: PathBuf::from("README.md"),
                contents: manifest::readme(&model),
            },
            GeneratedFile {
                path: PathBuf::from("src").join("lib.rs"),
                contents: render::lib_rs(&model),
            },
        ],
    })
}

/// The file name the generated hybrid crate embeds its manifest under.
///
/// Beside the `.kbc` at the crate root, for the same relocatability reason
/// [`artifact_file_name`] gives.
pub fn manifest_file_name(library: &str) -> String {
    format!("{library}.khm")
}

/// What a native wrapper crate is generated from.
///
/// The native counterpart of [`WrapperSpec`]. It carries no content hash: the
/// native engine's stale-build guard is a *symbol*, checked at the consumer's
/// link, rather than data checked at load. See [`render_native`].
#[derive(Debug, Clone, Copy)]
pub struct NativeWrapperSpec<'a> {
    /// The library's package name, which is also the crate and module name.
    pub library: &'a str,
    /// The version to give the generated crate, from the library's manifest.
    pub version: &'a str,
    /// The export surface the wrapper offers methods for.
    pub exports: &'a ExportTable,
    /// The symbols the backend emitted that surface under.
    pub symbols: &'a NativeExportSurface,
    /// The Kira checkout the generated crate takes its path dependencies from.
    pub toolchain_root: &'a Path,
    /// The directory holding the static archive the generated `build.rs` links.
    pub archive_directory: &'a Path,
}

/// Generates the native-engine wrapper crate for one library.
pub fn generate_native(spec: &NativeWrapperSpec<'_>) -> Result<GeneratedCrate, WrapperError> {
    let model = render_native::NativeModel::build(spec.library, spec.exports, spec.symbols)?;
    Ok(GeneratedCrate {
        name: model.library.clone(),
        files: vec![
            GeneratedFile {
                path: PathBuf::from("Cargo.toml"),
                contents: manifest::native_cargo_toml(spec, &model),
            },
            GeneratedFile {
                path: PathBuf::from("README.md"),
                contents: manifest::native_readme(&model),
            },
            GeneratedFile {
                path: PathBuf::from("build.rs"),
                contents: render_native::build_rs(
                    &model,
                    &spec.archive_directory.display().to_string(),
                ),
            },
            GeneratedFile {
                path: PathBuf::from("src").join("lib.rs"),
                contents: render_native::lib_rs(&model),
            },
        ],
    })
}

/// The Rust name of an exported class, or a named placeholder when the index
/// points at nothing.
///
/// Unreachable: every renderer refuses an out-of-range class index before
/// anything is rendered. Named rather than unwrapped, because a generator never
/// gets to end its caller's process — and shared by both engines' renderers so
/// there is one answer to what a broken index produces.
pub(crate) fn class_name_of(class: Option<&render::ClassModel>, index: u32) -> String {
    match class {
        Some(class) => class.rust.clone(),
        None => format!("UnknownClass{index}"),
    }
}

/// Which engine a generated crate was written for.
///
/// The two engines write into the same directory — `.kira-build/rust/<name>/` —
/// because a consumer's `path` dependency names that directory and must not move
/// when the library is rebuilt for the other engine. That makes leftovers a real
/// hazard, which is what [`foreign_engine_files`] exists to answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    /// Bytecode embedded in the crate and run on the VM.
    Vm,
    /// A static archive the crate links.
    Native,
    /// Bytecode and a manifest embedded in the crate, plus a shared library
    /// found at load.
    Hybrid,
}

/// The files the *other* engine leaves behind in a generated crate's directory.
///
/// Both engines overwrite `Cargo.toml`, `README.md`, and `src/lib.rs`, so those
/// take care of themselves. What does not is each engine's own extra file: the
/// native engine's `build.rs` and the VM engine's embedded `.kbc`. Left in
/// place, the `build.rs` is worse than clutter — cargo auto-detects a build
/// script by its presence, so a crate rebuilt for the VM would still run the
/// native engine's script and link a stale archive into the consumer's binary,
/// silently and against code nobody rebuilt.
///
/// Returned as data rather than deleted here: this module touches no filesystem,
/// so the two callers that write the crate are the two that remove them.
pub fn foreign_engine_files(engine: Engine, library: &str) -> Vec<PathBuf> {
    let bytecode = PathBuf::from(artifact_file_name(library));
    let manifest = PathBuf::from(manifest_file_name(library));
    let build_script = PathBuf::from("build.rs");
    match engine {
        // Two of the three engines embed the bytecode, so it is never stale
        // clutter for the VM engine — but the hybrid engine's manifest is, and
        // a `.khm` describing a split beside a `.kbc` that has none would be a
        // confusing thing to find.
        Engine::Vm => vec![build_script, manifest],
        Engine::Native => vec![bytecode, manifest],
        Engine::Hybrid => vec![build_script],
    }
}

/// The file name the generated crate embeds its artifact under.
///
/// At the crate root rather than beside `lib.rs`, so `include_bytes!` reads
/// `../<name>.kbc` and the crate stays relocatable: copy the directory anywhere
/// and it still finds its bytecode.
pub fn artifact_file_name(library: &str) -> String {
    format!("{library}.kbc")
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
