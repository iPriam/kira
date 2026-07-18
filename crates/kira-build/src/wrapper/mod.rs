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
mod render;

use std::path::{Path, PathBuf};

use kira_bytecode::ExportTable;

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

/// Generates the wrapper crate for one library.
pub fn generate(spec: &WrapperSpec<'_>) -> Result<GeneratedCrate, WrapperError> {
    let model = render::Model::build(spec)?;
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
