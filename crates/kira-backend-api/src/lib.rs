//! Backend-neutral compile request/result API implemented by code-generation backends.
//!
//! Layer 3 of the Kira package graph. This is the seam every code-generation
//! backend (VM bytecode, LLVM/native, hybrid) implements. It is deliberately
//! minimal: the concrete program input type is supplied by [`Backend::compile`]
//! once the IR is designed.

use kira_native_lib_definition::{ImportResolveError, ResolvedNativeLibraries, TargetTriple};
use kira_runtime_abi::ForeignImport;

/// Which artifact family a backend emits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BackendMode {
    /// VM bytecode for the interpreter.
    VmBytecode,
    /// Native code via the LLVM backend.
    LlvmNative,
    /// A hybrid bundle (bytecode plus native entry points).
    Hybrid,
}

impl BackendMode {
    /// This mode's spelling on the command line.
    ///
    /// The one place a mode becomes text, so a diagnostic naming a backend and
    /// the flag a user typed cannot drift apart.
    pub fn label(self) -> &'static str {
        match self {
            Self::VmBytecode => "vm",
            Self::LlvmNative => "llvm",
            Self::Hybrid => "hybrid",
        }
    }
}

/// Which WebAssembly memory a Web build targets.
///
/// A Kira program does not change shape between the two — `Int` is 64-bit
/// either way — so this widens what is addressable, not what is computable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WasmDevice {
    /// `wasm32`: the baseline 32-bit memory, which every engine has.
    Wasm32,
    /// `wasm64`: the Memory64 proposal's 64-bit memory.
    Wasm64,
}

impl WasmDevice {
    /// The device's spelling on the command line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Wasm32 => "wasm32",
            Self::Wasm64 => "wasm64",
        }
    }

    /// Resolves a `--device` value.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "wasm32" => Some(Self::Wasm32),
            "wasm64" => Some(Self::Wasm64),
            _ => None,
        }
    }
}

/// Output paths for native/LLVM emission.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NativeEmitOptions {
    /// Where the object file is written.
    pub object_path: String,
    /// Where the linked executable is written, if one is requested.
    pub executable_path: Option<String>,
    /// Where a shared library is written, if one is requested.
    pub shared_library_path: Option<String>,
    /// Where textual IR is written, if requested.
    pub ir_path: Option<String>,
}

/// A backend-neutral compile request.
#[derive(Debug, Clone)]
pub struct CompileRequest {
    /// The artifact family to emit.
    pub mode: BackendMode,
    /// The module name to record in emitted artifacts.
    pub module_name: String,
    /// Output paths for native/LLVM emission.
    pub emit: NativeEmitOptions,
    /// The resolved native libraries to link and the target to select their
    /// artifacts for. `None` for a build with no foreign imports.
    pub foreign_link: Option<ForeignLinkInput>,
}

/// The resolved native-link inputs a backend needs to satisfy foreign imports.
///
/// Carries the resolved catalog together with the selected target, so a backend
/// both validates imports and selects each import's archive against the one
/// target this build is for.
#[derive(Debug, Clone)]
pub struct ForeignLinkInput {
    /// The resolved native-library catalog, keyed by interned library name.
    pub native_libraries: ResolvedNativeLibraries,
    /// The target these libraries' artifacts are selected for.
    pub target: TargetTriple,
}

/// Why a foreign import could not be satisfied by the resolved catalog.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForeignLinkError {
    /// An import named a library the build does not declare.
    #[error("foreign import names native library `{library}`, not declared for target `{target}`")]
    UndeclaredLibrary {
        /// The undeclared library name.
        library: String,
        /// The target this build selected.
        target: TargetTriple,
    },
    /// A declared library has no artifact for the selected target.
    #[error("native library `{library}` has no native artifact for target `{target}`")]
    NoArtifactForTarget {
        /// The declared library missing an artifact.
        library: String,
        /// The target with no matching artifact.
        target: TargetTriple,
    },
    /// The catalog could not intern another distinct library name.
    #[error("too many distinct native-library names to intern for library `{library}`")]
    NameSpaceExhausted {
        /// The library whose name could not be interned.
        library: String,
    },
}

/// Validates every foreign import against the resolved catalog and target.
///
/// For each import, interns its library name into the catalog's own interner
/// (see the interner-ownership note on [`ResolvedNativeLibraries`]) and resolves
/// it for `target`, surfacing an undeclared library or a target with no artifact
/// as a typed [`ForeignLinkError`] naming the library and target. Takes the
/// catalog by `&mut` because interning a name is how a `String` import key
/// becomes the interned `Symbol` the catalog is keyed by — no separate interner
/// is passed, and none can drift out of sync, because the catalog owns the only
/// one involved.
pub fn validate_foreign_imports(
    imports: &[ForeignImport],
    catalog: &mut ResolvedNativeLibraries,
    target: &TargetTriple,
) -> Result<(), ForeignLinkError> {
    for import in imports {
        let symbol = catalog.intern_library(import.library()).map_err(|_| {
            ForeignLinkError::NameSpaceExhausted {
                library: import.library().to_owned(),
            }
        })?;
        catalog
            .resolve_import(symbol, target)
            .map_err(|error| match error {
                ImportResolveError::UndeclaredLibrary { library } => {
                    ForeignLinkError::UndeclaredLibrary {
                        library,
                        target: target.clone(),
                    }
                }
                ImportResolveError::NoArtifactForTarget { library, target } => {
                    ForeignLinkError::NoArtifactForTarget { library, target }
                }
            })?;
    }
    Ok(())
}

/// Kind of an emitted artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ArtifactKind {
    /// VM bytecode.
    Bytecode,
    /// A native object file.
    NativeObject,
    /// A native shared/static library.
    NativeLibrary,
    /// A linked executable.
    Executable,
    /// A hybrid bundle.
    HybridBundle,
}

/// One emitted artifact.
#[derive(Debug, Clone, PartialEq)]
pub struct Artifact {
    /// What kind of artifact this is.
    pub kind: ArtifactKind,
    /// Where the artifact was written.
    pub path: String,
}

/// A compile result: the set of artifacts a backend emitted.
#[derive(Debug, Clone, Default)]
pub struct CompileResult {
    /// The emitted artifacts.
    pub artifacts: Vec<Artifact>,
}

/// A backend failure.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct BackendError(pub String);

/// A code-generation backend.
///
/// The verified program input is threaded through once the IR is designed;
/// until then the trait fixes the request/result contract every backend shares.
pub trait Backend {
    /// Compiles per `request`, returning the emitted artifacts.
    fn compile(&mut self, request: &CompileRequest) -> Result<CompileResult, BackendError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_native_lib_definition::{
        Interner, LinkMode, NativeLibrarySpec, NativeTargetSpec, ResolvedNativeLibrary,
    };
    use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType};
    use std::path::Path;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    fn catalog(name: &str, rows: &[(&str, &str)]) -> ResolvedNativeLibraries {
        let targets = rows
            .iter()
            .map(|(t, path)| NativeTargetSpec::static_archive(triple(t), *path))
            .collect();
        let resolved: ResolvedNativeLibrary =
            NativeLibrarySpec::new(name, LinkMode::Static, targets)
                .expect("a valid declaration")
                .resolve(Path::new("/pkg/NativeLibs"), |_| true)
                .expect("resolution");
        // The catalog owns its interner; a fresh, empty one seeds it.
        ResolvedNativeLibraries::from_resolved(Interner::new(), vec![resolved]).expect("a catalog")
    }

    fn import(library: &str) -> ForeignImport {
        ForeignImport::new(
            library,
            "some_symbol",
            ForeignAbi::C,
            ForeignSignature::scalars([], ForeignType::Void),
        )
    }

    #[test]
    fn validation_passes_for_a_declared_library_with_a_matching_target() {
        let mut catalog = catalog("ffimath", &[("aarch64-macos-none", "lib/host.a")]);
        let result = validate_foreign_imports(
            &[import("ffimath")],
            &mut catalog,
            &triple("aarch64-macos-none"),
        );
        assert_eq!(result, Ok(()));
    }

    #[test]
    fn validation_fails_typed_for_an_undeclared_library() {
        let mut catalog = catalog("ffimath", &[("aarch64-macos-none", "lib/host.a")]);
        let error = validate_foreign_imports(
            &[import("othermath")],
            &mut catalog,
            &triple("aarch64-macos-none"),
        )
        .expect_err("an undeclared library is rejected");
        assert!(matches!(error, ForeignLinkError::UndeclaredLibrary { .. }));
        assert!(error.to_string().contains("othermath"));
    }

    #[test]
    fn validation_fails_typed_for_a_host_only_library_on_wasm() {
        let mut catalog = catalog("ffimath", &[("aarch64-macos-none", "lib/host.a")]);
        let wasm = triple("wasm32-emscripten-unknown");
        let error = validate_foreign_imports(&[import("ffimath")], &mut catalog, &wasm)
            .expect_err("a host-only library on wasm is rejected");
        assert_eq!(
            error,
            ForeignLinkError::NoArtifactForTarget {
                library: "ffimath".to_owned(),
                target: wasm,
            }
        );
    }
}
