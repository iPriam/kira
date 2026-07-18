//! The hybrid half of `build` and `run`: emitting a bundle, and running it.
//!
//! A hybrid build writes three artifacts into `.kira-build/`, from one IR:
//!
//! - `<stem>.kbc` — the bytecode half, every non-`@Native` function,
//! - `lib<stem>.dylib` (or `.so`) — the native half, one trampoline per
//!   `@Native` function,
//! - `<stem>.khm` — the manifest tying them together, which a run loads first.
//!
//! # Why the manifest is built here
//!
//! The manifest describes a program in the seam's own vocabulary
//! ([`BridgeValueTag`], [`Execution`]), and building one means reading the IR.
//! `kira-hybrid-definition` sits below `kira-ir` and must not learn about it, so
//! the composition happens in the CLI — which already depends on both — rather
//! than by giving a lower layer a dependency it should not have.
//!
//! # Agreeing with both halves
//!
//! Every engine assignment here resolves `Inherited` against
//! [`Execution::Runtime`], exactly as `kira_bytecode::compile_hybrid` and the
//! LLVM backend's `build_hybrid` do. The three must agree function for function:
//! the manifest is what the host marshals against, and a disagreement is what
//! the runtime's own bundle validation exists to catch.

use std::path::{Path, PathBuf};

use kira_hybrid_definition::{HybridFunction, HybridManifest, HybridParam};
use kira_ir::IrProgram;
use kira_llvm_backend::NativeBuildOptions;
use kira_runtime_abi::{BridgeValueTag, Execution};
use kira_semantics_model::Type;

use crate::native::{self, Artifacts, NativeError};

/// The artifacts a hybrid build produced.
pub struct HybridBundle {
    /// The manifest a run loads first.
    pub manifest: PathBuf,
}

/// Why a hybrid build or run failed.
#[derive(Debug, thiserror::Error)]
pub enum HybridError {
    /// The native half of the build failed.
    #[error(transparent)]
    Native(#[from] NativeError),
    /// The bytecode half of the build failed.
    #[error("bytecode compilation failed: {0}")]
    Bytecode(#[from] kira_bytecode::CompileError),
    /// The LLVM backend failed.
    #[error(transparent)]
    Backend(#[from] kira_llvm_backend::LlvmError),
    /// An artifact could not be written.
    #[error("cannot write `{path}`: {source}")]
    Io {
        /// The path being written.
        path: PathBuf,
        /// The underlying I/O failure.
        #[source]
        source: std::io::Error,
    },
    /// A function's signature uses a type the seam cannot carry.
    #[error("function `{function}` has a type the hybrid boundary cannot carry: {ty:?}")]
    UnsupportedType {
        /// The function whose signature cannot be described.
        function: String,
        /// The type that has no bridge tag.
        ty: Type,
    },
    /// Loading or running the bundle failed.
    #[error(transparent)]
    Runtime(#[from] kira_hybrid_runtime::HybridError),
}

/// Builds `program` into a hybrid bundle under `.kira-build/`.
pub fn build(
    program: &IrProgram,
    source: &Path,
    emit_llvm_ir: bool,
) -> Result<HybridBundle, HybridError> {
    let artifacts =
        Artifacts::for_source(source).map_err(|source| NativeError::Layout { source })?;

    // The bytecode half: every function that is not `@Native`.
    let module = kira_bytecode::compile_hybrid(program)?;
    let bytecode_path = artifacts.bytecode();
    write(&bytecode_path, &module.to_bytes())?;

    // The native half: one trampoline per `@Native` function.
    let options = NativeBuildOptions {
        module_name: artifacts.stem().to_owned(),
        object_path: artifacts.object(),
        // A hybrid program has no entrypoint of its own to link: the host is
        // the executable, and this half is a library it loads.
        executable_path: None,
        shared_library_path: Some(artifacts.shared_library()),
        ir_path: emit_llvm_ir.then(|| artifacts.llvm_ir()),
        runtime_archive: native::runtime_archive()?,
    };
    let native = kira_llvm_backend::build_hybrid_library(program, &options)?;

    // The manifest, last: it names the two payloads above.
    let manifest = manifest(program, &artifacts, &native.exports)?;
    let manifest_path = artifacts.manifest();
    write(&manifest_path, &manifest.to_bytes())?;

    Ok(HybridBundle {
        manifest: manifest_path,
    })
}

/// Builds a hybrid bundle and runs it, returning the program's exit code.
pub fn run(program: &IrProgram, source: &Path, emit_llvm_ir: bool) -> Result<i32, HybridError> {
    let bundle = build(program, source, emit_llvm_ir)?;
    let session = kira_hybrid_runtime::Session::load(&bundle.manifest)?;
    session.run()?;
    Ok(0)
}

/// Describes `program` as a manifest, given the trampolines the backend
/// exported.
fn manifest(
    program: &IrProgram,
    artifacts: &Artifacts,
    exports: &[(u32, String)],
) -> Result<HybridManifest, HybridError> {
    let functions = program
        .functions
        .iter()
        .enumerate()
        .map(|(index, function)| {
            let id = index as u32;
            let execution = function.execution.resolve(Execution::Runtime);
            let params = function
                .locals
                .iter()
                .take(function.param_count as usize)
                .map(|ty| {
                    // v0's IR carries no per-parameter mode — there is no borrow
                    // syntax yet — and the codegen frees every string parameter
                    // at return. `Owned` is what that is.
                    tag(*ty, &function.name).map(HybridParam::owned)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(HybridFunction {
                id,
                name: function.name.clone(),
                execution,
                params,
                returns: tag(function.return_type, &function.name)?,
                // Looked up rather than re-derived: the backend is what named
                // the symbol it emitted, so the manifest records that name
                // rather than a second guess at it.
                exported_name: exports
                    .iter()
                    .find(|(exported, _)| *exported == id)
                    .map(|(_, symbol)| symbol.clone()),
            })
        })
        .collect::<Result<Vec<_>, HybridError>>()?;

    Ok(HybridManifest {
        module_name: artifacts.stem().to_owned(),
        bytecode_path: file_name(&artifacts.bytecode()),
        native_library_path: file_name(&artifacts.shared_library()),
        entry: program.main,
        functions,
    })
}

/// The bridge tag for an IR type, or why it cannot cross.
fn tag(ty: Type, function: &str) -> Result<BridgeValueTag, HybridError> {
    Ok(match ty {
        // Every integer width crosses the seam as one tag, because every
        // integer width *is* one 64-bit representation. The written width is a
        // front-end distinction; the bridge carries values, not spellings.
        Type::Int(_) => BridgeValueTag::INT,
        Type::Float(_) => BridgeValueTag::FLOAT,
        Type::Bool => BridgeValueTag::BOOL,
        Type::String => BridgeValueTag::STRING,
        Type::Void => BridgeValueTag::VOID,
        // Described, not carried. A manifest has a row for every function in
        // the program, and most of them never cross: rejecting a struct here
        // would reject a `@Runtime` function that merely *has* one in its
        // signature and is only ever called from other `@Runtime` code. What a
        // struct cannot do is travel, and that is enforced where a crossing is
        // actually emitted — the backend refuses to build one whose signature
        // mentions a struct.
        Type::Struct(_) => BridgeValueTag::STRUCT,
        // Described, not carried, for the same reason a struct is — though not
        // on the same grounds. See `BridgeValueTag::ARRAY`: the language lets
        // an array cross, and what is missing is the ownership answer at the
        // boundary, not a place to put it.
        Type::Array(_) => BridgeValueTag::ARRAY,
        // Described, not carried, for the same reason a struct is: an enum is a
        // tagged value that does not fit one tag and one word, and how it would
        // cross is a language decision nobody has made. A `@Runtime` function
        // may merely mention one in its signature.
        Type::Enum(_) => BridgeValueTag::ENUM,
        // A verified IR carries no `Error` type — reaching one means the
        // frontend let a broken program through, which is a compiler bug, not
        // something to encode into an artifact.
        Type::Error => {
            return Err(HybridError::UnsupportedType {
                function: function.to_owned(),
                ty,
            });
        }
    })
}

/// The file name of `path`, which is how a manifest records a payload.
///
/// Recorded relative rather than absolute so the bundle can be moved as a unit;
/// the runtime resolves each against the manifest's own directory, and all
/// three artifacts share one.
fn file_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Writes an artifact, naming it on failure.
fn write(path: &Path, bytes: &[u8]) -> Result<(), HybridError> {
    std::fs::write(path, bytes).map_err(|source| HybridError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_v0_type_has_a_bridge_tag() {
        assert_eq!(tag(Type::INT, "f").expect("int"), BridgeValueTag::INT);
        assert_eq!(tag(Type::FLOAT, "f").expect("float"), BridgeValueTag::FLOAT);
        assert_eq!(tag(Type::Bool, "f").expect("bool"), BridgeValueTag::BOOL);
        assert_eq!(
            tag(Type::String, "f").expect("string"),
            BridgeValueTag::STRING
        );
        assert_eq!(tag(Type::Void, "f").expect("void"), BridgeValueTag::VOID);
    }

    #[test]
    fn the_error_type_is_refused_rather_than_encoded() {
        let error = tag(Type::Error, "broken").expect_err("the error type cannot cross");
        assert!(
            matches!(
                error,
                HybridError::UnsupportedType {
                    ty: Type::Error,
                    ..
                }
            ),
            "{error:?}",
        );
    }

    #[test]
    fn a_payload_is_recorded_by_file_name_so_a_bundle_can_move() {
        assert_eq!(file_name(Path::new("/tmp/build/demo.kbc")), "demo.kbc");
    }
}
