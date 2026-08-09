//! Building a Kira library for the **hybrid engine**, and the two refusals it
//! owes.
//!
//! # What `@Runtime` and `@Native` mean for a library
//!
//! The split was built to serve live-reload of applications, and a library has
//! no application to reload — so what it means here is a real question rather
//! than an inherited answer. The answer is that **it means exactly what it
//! always meant: which engine runs a function's body.** What a library changes
//! is not the split but the *entry*: an application is entered at its `@Main`, a
//! library is entered by its consumer, one call at a time.
//!
//! That is what makes this engine worth building rather than a parity checkbox.
//! The VM engine compiles every function to bytecode; the native engine compiles
//! every function to machine code; neither is wrong, and neither lets the
//! library's author choose per function. **The hybrid engine is the only one
//! that honors the annotation**, so it is the only one where a library's hot
//! inner function is machine code while its surface, its handles, and its
//! strings stay on the VM.
//!
//! # Three artifacts, and where each one goes
//!
//! ```text
//! .kira-build/lib/uifoundation.kbc          the bytecode half
//! .kira-build/lib/uifoundation.khm          the split describing both halves
//! .kira-build/lib/libuifoundation.dylib     the native half
//! .kira-build/rust/uifoundation/            the crate a Rust program depends on
//! ```
//!
//! The first two are **data**, so the generated crate embeds copies of both and
//! stays relocatable exactly as the VM engine's does. The third is code a
//! process must `dlopen`, so it stays a file — which makes deployment exactly
//! one file long, and `kira_hybrid_main::locate` is where being found at load
//! time is designed.
//!
//! # The two refusals, and why they are here rather than in the frontend
//!
//! Both are consequences of *this engine*, not of the language, so they belong
//! beside the engine that has them rather than above the backend split where
//! they would refuse programs the other two engines build happily.
//!
//! - **An `@Export` function may not be `@Native`.** A handle is a root into the
//!   instance's VM heap; machine code cannot mint one. A `@Native` export
//!   returning a class would allocate in a second heap and the consumer would
//!   hold two different things behind one newtype with one destructor. Refused
//!   for the whole surface rather than only for exports that mention a class,
//!   because "this export may be `@Native` and that one may not" is a rule
//!   nobody can hold in their head.
//! - **A `@Native` function may not call a `@Runtime` one.** A library instance
//!   owns a heap and is called through `&mut self`; a call back into it from
//!   inside an exported call would need a second mutable borrow of the same
//!   instance. An *application's* hybrid session has no such problem because it
//!   runs on a `Program`, which holds no heap. So this direction of the seam is
//!   the one thing a hybrid library gives up, and it gives it up by name at
//!   build time rather than by aborting at run time.

use std::path::{Path, PathBuf};

use kira_hybrid_definition::{HybridFunction, HybridManifest, HybridParam};
use kira_ir::IrProgram;
use kira_runtime_abi::{BridgeValueTag, Execution, Ownership};
use kira_semantics_model::Type;

use crate::wrapper::{self, WrapperSpec};

/// Where a hybrid library build writes, and what it is called.
#[derive(Debug, Clone)]
pub struct HybridLibraryOptions {
    /// The library's package name: every artifact's name, and the crate's.
    pub name: String,
    /// The library's version, from its manifest.
    pub version: String,
    /// The `.kira-build` directory to write under.
    pub build_directory: PathBuf,
    /// The Kira checkout the generated crate takes its path dependencies from.
    pub toolchain_root: PathBuf,
    /// The native runtime archive to bake into the native half.
    pub runtime_archive: PathBuf,
    /// Whether to write the textual LLVM IR beside the object, for debugging.
    pub emit_llvm_ir: bool,
}

/// What a hybrid library build produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HybridLibraryArtifacts {
    /// The bytecode half.
    pub bytecode: PathBuf,
    /// The manifest describing the split.
    pub manifest: PathBuf,
    /// The native half, which is the one file a deployment must carry.
    pub native_half: PathBuf,
    /// The root of the generated wrapper crate.
    pub wrapper_crate: PathBuf,
    /// How many exports the wrapper offers a method for.
    pub exports: usize,
    /// How many functions run as machine code rather than bytecode.
    ///
    /// Reported because it is the number that says whether this engine bought
    /// the author anything: a library with none of them is a VM-engine library
    /// that also needs a `.dylib` deployed beside it.
    pub native_functions: usize,
}

/// Why a hybrid library could not be built.
#[derive(Debug, thiserror::Error)]
pub enum HybridLibraryError {
    /// The bytecode half did not compile.
    #[error("bytecode compilation failed: {0}")]
    Compile(#[from] kira_bytecode::CompileError),
    /// The native half's backend failed, or is not compiled into this build.
    #[error(transparent)]
    Backend(#[from] kira_llvm_backend::LlvmError),
    /// The export surface has no legal Rust spelling.
    #[error("this library's export surface cannot be generated for Rust: {0}")]
    Wrapper(#[from] wrapper::WrapperError),
    /// Internal invariant: the compiled bytecode half has fewer functions than
    /// the program it was compiled from.
    ///
    /// The compiler only ever appends to the function table, so this is a
    /// compiler bug surfaced typed rather than anything a program can cause.
    #[error(
        "bytecode compiler invariant violated: the program has {program} functions but the \
         bytecode half carries only {module}"
    )]
    BytecodeHalfLostFunctions {
        /// How many functions the program declares.
        program: usize,
        /// How many the compiled bytecode half carries.
        module: usize,
    },
    /// An exported function is annotated `@Native`.
    #[error(
        "`{function}` is both `@Export` and `@Native`, which the hybrid engine cannot build: \
         a consumer enters this library through its bytecode half, because a handle it \
         gets back is a root into that half's heap and machine code has no way to mint \
         one\n\
         note: drop `@Native` from `{function}` and put it on the function `{function}` \
         calls instead — that is the split this engine exists to honor"
    )]
    ExportIsNative {
        /// The function annotated both ways.
        function: String,
    },
    /// A native function calls back into the runtime half.
    #[error(
        "`{function}` is `@Native` and calls the `@Runtime` function `{callee}`, which a \
         hybrid *library* cannot do: a library instance owns a heap and is entered through \
         a mutable borrow, so a call back into it from inside an exported call would need \
         a second one\n\
         note: an application built with `--backend hybrid` may call in both directions; a \
         library may not, and this is the one thing it gives up\n\
         note: annotate `{callee}` `@Native` too, or move the call into a `@Runtime` \
         function that calls `{function}` rather than the other way around"
    )]
    NativeCallsRuntime {
        /// The native function making the call.
        function: String,
        /// The runtime function it calls.
        callee: String,
    },
    /// A function's signature uses a type the seam cannot describe.
    #[error("function `{function}` has a type the hybrid boundary cannot carry: {ty:?}")]
    UnsupportedType {
        /// The function whose signature cannot be described.
        function: String,
        /// The type that has no bridge tag.
        ty: Type,
    },
    /// An artifact could not be written.
    #[error("cannot write `{path}`: {source}")]
    Write {
        /// The path that could not be written.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// The build directory has no absolute spelling.
    ///
    /// The path baked into the generated crate is read at *run* time, from
    /// wherever the consumer's binary happens to be, so a relative one would
    /// resolve against a directory nobody chose.
    #[error("cannot resolve `{path}` to an absolute path: {source}")]
    Unresolvable {
        /// The path that could not be resolved.
        path: String,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
}

/// The engine each function's body runs on, `Inherited` resolved.
///
/// Resolved against [`Execution::Runtime`] exactly as `compile_hybrid` and the
/// LLVM backend's `build_hybrid` do. The three must agree function for function
/// — the manifest is what every crossing marshals against — so there is one
/// spelling of the rule and this is it.
pub fn engines(program: &IrProgram) -> Vec<Execution> {
    program
        .functions
        .iter()
        .map(|function| function.execution.resolve(Execution::Runtime))
        .collect()
}

/// Refuses the two programs the hybrid engine cannot build as a library.
///
/// Run before anything is compiled or written, so a refusal names a function
/// while there is still nothing on disk to be confused by.
pub fn check_library(program: &IrProgram) -> Result<(), HybridLibraryError> {
    let engines = engines(program);

    for export in &program.exports {
        let index = export.function as usize;
        if engines.get(index).copied() == Some(Execution::Native) {
            return Err(HybridLibraryError::ExportIsNative {
                function: export.kira_name.clone(),
            });
        }
    }

    for (index, function) in program.functions.iter().enumerate() {
        if engines.get(index).copied() != Some(Execution::Native) {
            continue;
        }
        for callee in crate::callgraph::direct_calls(program, &function.body) {
            if engines.get(callee as usize).copied() == Some(Execution::Runtime) {
                return Err(HybridLibraryError::NativeCallsRuntime {
                    function: function.name.clone(),
                    callee: program
                        .functions
                        .get(callee as usize)
                        .map(|target| target.name.clone())
                        .unwrap_or_else(|| format!("#{callee}")),
                });
            }
        }
    }
    Ok(())
}

/// How many functions the compiled bytecode half carries beyond the program's.
///
/// The VM synthesizes widen helpers and appends them, so this is a subtraction
/// rather than a count of anything the IR holds. A module with *fewer*
/// functions than the program is a compiler bug, and is reported as one rather
/// than wrapping into an enormous count.
pub fn internal_function_count(
    program: &IrProgram,
    module: &kira_bytecode::module::Module,
) -> Result<u32, HybridLibraryError> {
    module
        .functions
        .len()
        .checked_sub(program.functions.len())
        .and_then(|extra| u32::try_from(extra).ok())
        .ok_or(HybridLibraryError::BytecodeHalfLostFunctions {
            program: program.functions.len(),
            module: module.functions.len(),
        })
}

/// Describes `program` as a `.khm` manifest, given the trampolines the backend
/// emitted.
///
/// `exports` is the backend's own `(function id, symbol)` list: the manifest
/// records the name the backend *emitted*, never a second guess at it.
///
/// `internal_functions` is how many functions the compiled bytecode half
/// carries beyond the program's own — the VM's synthesized widen helpers. It is
/// taken from the compiled module rather than recomputed, for the same reason
/// `exports` is taken from the backend: the manifest records what was built.
pub fn manifest(
    program: &IrProgram,
    module_name: &str,
    bytecode_file: &str,
    native_file: &str,
    exports: &[(u32, String)],
    internal_functions: u32,
) -> Result<HybridManifest, HybridLibraryError> {
    let functions = program
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            let id = index as u32;
            let params = function
                .locals
                .iter()
                .take(function.param_count as usize)
                .enumerate()
                .map(|(slot, ty)| {
                    // A written-through parameter is the one mode a crossing has
                    // to know about: its final value comes back in the slot it
                    // went out in, and this row is what tells the host which
                    // slots to read. Everything else is `Owned` — the codegen
                    // frees every string parameter at return, and a read-only
                    // borrow crosses as a copy the callee owns just the same.
                    let ownership = if function.param_by_reference(slot as u32) {
                        Ownership::BorrowMut
                    } else {
                        Ownership::Owned
                    };
                    tag(*ty, &function.name).map(|ty| HybridParam { ty, ownership })
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HybridFunction {
                id,
                name: function.name.clone(),
                execution: function.execution.resolve(Execution::Runtime),
                params,
                returns: tag(function.return_type, &function.name)?,
                exported_name: exports
                    .iter()
                    .find(|(exported, _)| *exported == id)
                    .map(|(_, symbol)| symbol.clone()),
            })
        })
        .collect::<Result<Vec<_>, HybridLibraryError>>()?;

    Ok(HybridManifest {
        module_name: module_name.to_owned(),
        bytecode_path: bytecode_file.to_owned(),
        native_library_path: native_file.to_owned(),
        // A library has no entrypoint by construction. The format already has a
        // sentinel for it, and this is the case it was added for.
        entry: program.main,
        functions,
        internal_functions,
        // One row per `@FFI.Extern` import, paired with the adapter symbol the
        // LLVM backend emits for that import index — the same symbol the hybrid
        // session resolves out of the native half. The name comes from
        // `kira_llvm_backend::adapter_name`, the one place that contract is
        // spelled, so producer and consumer cannot disagree.
        foreign: program
            .foreign_imports
            .iter()
            .enumerate()
            .map(|(index, import)| {
                kira_hybrid_definition::HybridForeign::from_import(
                    &import.import,
                    kira_llvm_backend::adapter_name(index),
                )
            })
            .collect(),
        foreign_aggregates: program.foreign_aggregates.clone(),
    })
}

/// The bridge tag for an IR type, or why it cannot be described.
///
/// Described, not carried: a manifest has a row for every function in the
/// program, and most of them never cross. Refusing a struct here would refuse a
/// `@Runtime` function that merely *mentions* one and is only ever called from
/// other `@Runtime` code. What a struct cannot do is travel, and that is
/// enforced where a crossing is emitted.
fn tag(ty: Type, function: &str) -> Result<BridgeValueTag, HybridLibraryError> {
    Ok(match ty {
        Type::Int(_) => BridgeValueTag::INT,
        Type::Float(_) => BridgeValueTag::FLOAT,
        Type::Bool => BridgeValueTag::BOOL,
        Type::String => BridgeValueTag::STRING,
        Type::Void => BridgeValueTag::VOID,
        Type::Struct(_) => BridgeValueTag::STRUCT,
        Type::Array(_) => BridgeValueTag::ARRAY,
        Type::Enum(_) => BridgeValueTag::ENUM,
        // A `RawPtr` is a first-class scalar a `@Native`/`@Runtime` signature may
        // name, so the manifest describes it with its own tag.
        Type::RawPtr | Type::ForeignPtr(_) => BridgeValueTag::RAW_PTR,
        // `CString` is seam-only — legal only as a foreign parameter — so it
        // never appears in a manifest row for an ordinary function.
        // Described like a struct, and travelling no more than one does: a
        // `@Runtime` function may mention `Any` and never cross, and a crossing
        // is refused where it is emitted rather than by refusing the row.
        Type::Any => BridgeValueTag::ANY,
        // A task handle names a row in the running program's own task table, so
        // it means nothing to the other engine and never crosses a hybrid seam.
        // A capture cell is shared mutable storage this engine counts holds on,
        // so it never crosses either — a hold taken on one side and released on
        // the other is a count neither engine owns. It is not surface, so no
        // signature an author writes reaches this arm.
        Type::CString | Type::NativeState(_) | Type::Task(_) | Type::Cell(_) => {
            return Err(HybridLibraryError::UnsupportedType {
                function: function.to_owned(),
                ty,
            });
        }
        // A verified IR carries no `Error` type: reaching one means the frontend
        // let a broken program through, which is a compiler bug rather than
        // something to encode into an artifact.
        Type::Error => {
            return Err(HybridLibraryError::UnsupportedType {
                function: function.to_owned(),
                ty,
            });
        }
    })
}

/// Compiles `program` as a hybrid library and generates the crate that calls it.
pub fn build_hybrid_library(
    program: &IrProgram,
    options: &HybridLibraryOptions,
) -> Result<HybridLibraryArtifacts, HybridLibraryError> {
    check_library(program)?;

    let lib_directory = options.build_directory.join("lib");
    std::fs::create_dir_all(&lib_directory).map_err(|source| HybridLibraryError::Write {
        path: lib_directory.display().to_string(),
        source,
    })?;

    // The bytecode half, which is also where the export table comes from: one
    // place decides what a library's export surface is, whatever engine is
    // being built.
    let module = kira_bytecode::compile_hybrid(program)?;
    let bytes = module.to_bytes();
    let content_hash = kira_main::content_hash(&bytes);
    let bytecode_file = wrapper::artifact_file_name(&options.name);
    let bytecode = lib_directory.join(&bytecode_file);
    write(&bytecode, &bytes)?;

    // The native half: one trampoline per `@Native` function, in a dylib rather
    // than an archive, because it is loaded rather than linked.
    let native_file = kira_hybrid_main::shared_library_file_name(&options.name);
    let native_half = lib_directory.join(&native_file);
    let built = kira_llvm_backend::build_hybrid_library(
        program,
        &kira_llvm_backend::NativeBuildOptions {
            module_name: options.name.clone(),
            object_path: lib_directory.join(format!("{}.o", options.name)),
            // No entrypoint to link: the consumer's program is the executable
            // and this half is a library it opens.
            executable_path: None,
            shared_library_path: Some(native_half.clone()),
            // Entered through per-function trampolines rather than through an
            // export surface: the consumer enters the *bytecode* half, so the
            // `kira_lib_*` family the native engine emits has no role here.
            archive_path: None,
            exports: kira_llvm_backend::NativeExportSurface::default(),
            ir_path: options
                .emit_llvm_ir
                .then(|| lib_directory.join(format!("{}.ll", options.name))),
            runtime_archive: options.runtime_archive.clone(),
            // Foreign link inputs reach the hybrid native half through the
            // hybrid build path, not this base options struct.
            foreign_link: kira_llvm_backend::NativeLinkInputs::EMPTY,
            optimize: false,
            unavailable_imports: Vec::new(),
        },
    )?;

    // The manifest, last of the three: it names the other two.
    let manifest_file = wrapper::manifest_file_name(&options.name);
    let manifest_path = lib_directory.join(&manifest_file);
    let described = manifest(
        program,
        &options.name,
        &bytecode_file,
        &native_file,
        &built.exports,
        internal_function_count(program, &module)?,
    )?;
    let manifest_bytes = described.to_bytes();
    write(&manifest_path, &manifest_bytes)?;

    // Absolute, and resolved after the files exist so the answer is real rather
    // than lexical: this path is read at *run* time from wherever the consumer's
    // binary is, which is not where `kira` was invoked.
    let resolved = std::fs::canonicalize(&lib_directory).map_err(|source| {
        HybridLibraryError::Unresolvable {
            path: lib_directory.display().to_string(),
            source,
        }
    })?;

    let generated = wrapper::generate_hybrid(
        &WrapperSpec {
            library: &options.name,
            version: &options.version,
            exports: &module.exports,
            content_hash,
            toolchain_root: &options.toolchain_root,
        },
        &manifest_file,
        &resolved.join(&native_file),
    )?;

    let wrapper_crate = options.build_directory.join("rust").join(&generated.name);
    for file in &generated.files {
        write(&wrapper_crate.join(&file.path), file.contents.as_bytes())?;
    }
    // Both embedded copies, at the crate root, so `include_bytes!` reads
    // `../<name>.kbc` and `../<name>.khm` and the crate relocates as a unit.
    write(&wrapper_crate.join(&bytecode_file), &bytes)?;
    write(&wrapper_crate.join(&manifest_file), &manifest_bytes)?;
    remove_foreign_engine_files(&wrapper_crate, &options.name)?;

    let native_functions = engines(program)
        .iter()
        .filter(|execution| **execution == Execution::Native)
        .count();

    Ok(HybridLibraryArtifacts {
        bytecode,
        manifest: manifest_path,
        native_half,
        wrapper_crate,
        exports: module.exports.functions.len(),
        native_functions,
    })
}

/// Removes what another engine left in this crate directory.
fn remove_foreign_engine_files(
    wrapper_crate: &Path,
    library: &str,
) -> Result<(), HybridLibraryError> {
    for file in wrapper::foreign_engine_files(wrapper::Engine::Hybrid, library) {
        let path = wrapper_crate.join(file);
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(HybridLibraryError::Write {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
    }
    Ok(())
}

/// Writes `contents` to `path`, creating the directories above it.
fn write(path: &Path, contents: &[u8]) -> Result<(), HybridLibraryError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| HybridLibraryError::Write {
            path: parent.display().to_string(),
            source,
        })?;
    }
    std::fs::write(path, contents).map_err(|source| HybridLibraryError::Write {
        path: path.display().to_string(),
        source,
    })
}

#[cfg(test)]
#[path = "hybrid_tests.rs"]
mod tests;
