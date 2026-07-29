//! Building a Kira library for the **native engine**, and the symbols that
//! makes visible.
//!
//! The VM engine's product is a `.kbc` plus a crate that embeds it
//! ([`crate::library`]). The native engine's product is a static archive plus a
//! crate that *links* it — the same generated API over a different engine, which
//! is the whole point: a consumer's code does not change when the engine does.
//!
//! # This module owns the symbol derivation, and nothing else does
//!
//! Every `kira_lib_*` name has one correct spelling and `kira-main` owns it.
//! Here is where that spelling is asked for, once, and handed in two directions:
//! down to the backend, which emits those symbols, and across to the generator,
//! which declares them in the consumer's `extern` block. A second derivation
//! anywhere would be a symbol-drift bug waiting for a rename.

use std::path::{Path, PathBuf};

use kira_bytecode::ExportTable;
use kira_ir::IrProgram;
use kira_llvm_backend::{NativeClass, NativeExport, NativeExportSurface};

/// Where a native library build writes, and what it is called.
#[derive(Debug, Clone)]
pub struct NativeLibraryOptions {
    /// The library's package name: the archive's name, and the crate's.
    pub name: String,
    /// The library's version, from its manifest.
    pub version: String,
    /// The `.kira-build` directory to write under.
    pub build_directory: PathBuf,
    /// The Kira checkout the generated crate takes its path dependencies from.
    pub toolchain_root: PathBuf,
    /// The native runtime archive to bake into the library's own archive.
    pub runtime_archive: PathBuf,
    /// Whether to write the textual LLVM IR beside the object, for debugging.
    pub emit_llvm_ir: bool,
}

/// What a native library build produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLibraryArtifacts {
    /// The static archive a Rust consumer links.
    pub archive: PathBuf,
    /// The root of the generated wrapper crate.
    pub wrapper_crate: PathBuf,
    /// How many exports the wrapper offers a method for.
    pub exports: usize,
}

/// Why a native library could not be built.
#[derive(Debug, thiserror::Error)]
pub enum NativeLibraryError {
    /// The program did not compile far enough to know its export surface.
    #[error("bytecode compilation failed: {0}")]
    Compile(#[from] kira_bytecode::CompileError),
    /// The native backend failed, or is not compiled into this build.
    #[error(transparent)]
    Backend(#[from] kira_llvm_backend::LlvmError),
    /// The export surface has no legal Rust spelling.
    #[error("this library's export surface cannot be generated for Rust: {0}")]
    Wrapper(#[from] crate::wrapper::WrapperError),
    /// An artifact could not be written.
    #[error("cannot write `{path}`: {source}")]
    Write {
        /// The path that could not be written.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The archive directory has no absolute spelling.
    ///
    /// The generated `build.rs` runs from the generated crate's own directory,
    /// wherever a consumer put it, so a relative link-search path in it would
    /// resolve against the wrong directory and fail the consumer's link on a
    /// library that is right where it was left.
    #[error("cannot resolve `{path}` to an absolute path: {source}")]
    Unresolvable {
        /// The path that could not be resolved.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// The `kira_lib_*` surface a library of this name and shape exports.
///
/// Derived from the *export table* rather than from the IR, so the class list is
/// the one the consumer's wrapper indexes: classes appear in first-mention order
/// across the export signatures, which `kira-bytecode` decides and nothing here
/// re-decides.
pub fn export_surface(library: &str, exports: &ExportTable) -> NativeExportSurface {
    NativeExportSurface {
        abi_marker: Some(kira_main::export_abi_marker(library)),
        functions: exports
            .functions
            .iter()
            .map(|export| NativeExport {
                symbol: kira_main::export_symbol(library, &export.name),
                function: export.function,
            })
            .collect(),
        classes: exports
            .classes
            .iter()
            .map(|class| NativeClass {
                // The consumer-facing spelling of a class is its Kira name
                // snake_cased, exactly as an export's is: `Button` becomes
                // `button`, so the destructor is `kira_lib_<lib>_drop_button`.
                symbol: kira_main::class_drop_symbol(
                    library,
                    &kira_semantics::exported_name(class),
                ),
                name: class.clone(),
            })
            .collect(),
    }
}

/// The archive file name for a library, as a linker expects to find it.
///
/// `lib<name>.a` on every platform this targets; cargo's
/// `cargo:rustc-link-lib=static=<name>` looks for exactly that.
pub fn archive_file_name(library: &str) -> String {
    format!("lib{library}.a")
}

/// Compiles `program` as a native library and generates the crate that links it.
pub fn build_native_library(
    program: &IrProgram,
    options: &NativeLibraryOptions,
) -> Result<NativeLibraryArtifacts, NativeLibraryError> {
    // The export table comes from the bytecode compiler even though no bytecode
    // is shipped here: it is the one place that decides what a library's export
    // surface *is*, and two answers to that would be one too many. Compiling to
    // get it costs a fraction of the native build beside it.
    let module = kira_bytecode::compile(program)?;
    let surface = export_surface(&options.name, &module.exports);

    let lib_directory = options.build_directory.join("lib");
    // LLVM writes the object with the C API, which does not create directories
    // and reports a bare "No such file or directory" when one is missing.
    std::fs::create_dir_all(&lib_directory).map_err(|source| NativeLibraryError::Write {
        path: lib_directory.display().to_string(),
        source,
    })?;
    let archive = lib_directory.join(archive_file_name(&options.name));
    let object = lib_directory.join(format!("{}.o", options.name));
    kira_llvm_backend::build_native_library(
        program,
        &kira_llvm_backend::NativeBuildOptions {
            module_name: options.name.clone(),
            object_path: object,
            executable_path: None,
            // A Rust consumer links the archive. The dylib form is the other
            // shape a host might want and is not built here: nothing in this
            // path consumes it, and building an artifact nobody reads is how a
            // build gets slower for nothing.
            shared_library_path: None,
            archive_path: Some(archive.clone()),
            ir_path: options
                .emit_llvm_ir
                .then(|| lib_directory.join(format!("{}.ll", options.name))),
            runtime_archive: options.runtime_archive.clone(),
            exports: surface.clone(),
            foreign_link: kira_llvm_backend::NativeLinkInputs::EMPTY,
            optimize: false,
            unavailable_imports: Vec::new(),
        },
    )?;

    // Absolute, always. `kira` is run from wherever the author happens to be,
    // so `build_directory` can perfectly well be relative — and the path baked
    // into the generated `build.rs` is read by cargo from the *generated
    // crate's* directory, which is somewhere else entirely. Resolved after the
    // archive is built, so the directory exists and the answer is real rather
    // than lexical.
    let archive_directory = std::fs::canonicalize(&lib_directory).map_err(|source| {
        NativeLibraryError::Unresolvable {
            path: lib_directory.display().to_string(),
            source,
        }
    })?;

    let generated = crate::wrapper::generate_native(&crate::wrapper::NativeWrapperSpec {
        library: &options.name,
        version: &options.version,
        exports: &module.exports,
        symbols: &surface,
        toolchain_root: &options.toolchain_root,
        archive_directory: &archive_directory,
    })?;
    let wrapper_crate = options.build_directory.join("rust").join(&generated.name);
    for file in &generated.files {
        write(&wrapper_crate.join(&file.path), file.contents.as_bytes())?;
    }
    // The VM engine's embedded bytecode, if this package was built that way
    // before. Nothing in the native crate reads it, and a `.kbc` sitting beside
    // an archive is an invitation to believe the wrong engine is linked.
    for file in crate::wrapper::foreign_engine_files(crate::wrapper::Engine::Native, &options.name)
    {
        let path = wrapper_crate.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(NativeLibraryError::Write {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }

    Ok(NativeLibraryArtifacts {
        archive,
        wrapper_crate,
        exports: module.exports.functions.len(),
    })
}

/// Writes `contents` to `path`, creating the directories above it.
fn write(path: &Path, contents: &[u8]) -> Result<(), NativeLibraryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| NativeLibraryError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| NativeLibraryError::Write {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_bytecode::{ExportType, ModuleExport};

    fn table() -> ExportTable {
        ExportTable {
            classes: vec!["Button".to_owned()],
            functions: vec![ModuleExport {
                name: "make_button".to_owned(),
                kira_name: "makeButton".to_owned(),
                function: 3,
                params: vec![ExportType::String],
                result: ExportType::Handle { class: 0 },
            }],
        }
    }

    #[test]
    fn every_symbol_is_the_one_kira_main_spells() {
        let surface = export_surface("uifoundation", &table());
        assert_eq!(
            surface.abi_marker.as_deref(),
            Some("kira_lib_uifoundation_abi_1")
        );
        assert_eq!(
            surface.functions[0].symbol,
            "kira_lib_uifoundation_make_button"
        );
        assert_eq!(surface.functions[0].function, 3);
        assert_eq!(
            surface.classes[0].symbol,
            "kira_lib_uifoundation_drop_button"
        );
        // The backend resolves the class against the program's own struct
        // table, so the *declared* name has to survive the trip.
        assert_eq!(surface.classes[0].name, "Button");
    }

    #[test]
    fn a_library_that_exports_nothing_still_defines_its_marker() {
        // The wrapper calls the marker from `load()` whether or not there is
        // anything else to call, so a stale empty library must still fail the
        // link rather than link silently.
        let surface = export_surface("uifoundation", &ExportTable::default());
        assert!(!surface.is_empty());
        assert_eq!(
            surface.abi_marker.as_deref(),
            Some("kira_lib_uifoundation_abi_1")
        );
    }

    #[test]
    fn the_archive_is_named_the_way_a_linker_looks_for_it() {
        // `cargo:rustc-link-lib=static=uifoundation` searches for exactly this.
        assert_eq!(archive_file_name("uifoundation"), "libuifoundation.a");
    }
}
