//! The resolved-library catalog and foreign-import resolution against it.

use std::collections::HashMap;
use std::path::Path;

use kira_core::{Interner, Symbol};

use crate::manifest::{NativeLibraryError, ResolvedNativeLibrary};
use crate::triple::TargetTriple;

/// A set of resolved native libraries keyed by interned library name.
///
/// The catalog owns the [`Interner`] its keys come from (see the crate-level
/// docs). Build it with [`ResolvedNativeLibraries::from_resolved`], turn a
/// foreign import's library name into a key with
/// [`ResolvedNativeLibraries::intern_library`], and resolve it with
/// [`ResolvedNativeLibraries::resolve_import`].
#[derive(Debug, Clone)]
pub struct ResolvedNativeLibraries {
    interner: Interner,
    libraries: HashMap<Symbol, ResolvedNativeLibrary>,
}

impl ResolvedNativeLibraries {
    /// Builds a catalog from resolved libraries, interning their names.
    ///
    /// Takes ownership of the `interner` so the catalog can both key libraries
    /// by [`Symbol`] and, later, intern an import's library name into the very
    /// same interner for lookup. Two libraries sharing a name are a
    /// [`NativeLibraryError::DuplicateLibrary`].
    pub fn from_resolved(
        mut interner: Interner,
        libraries: Vec<ResolvedNativeLibrary>,
    ) -> Result<Self, NativeLibraryError> {
        let mut map = HashMap::with_capacity(libraries.len());
        for library in libraries {
            let symbol = interner
                .intern(library.name())
                .map_err(|_| NativeLibraryError::NameSpaceExhausted)?;
            if map.contains_key(&symbol) {
                return Err(NativeLibraryError::DuplicateLibrary {
                    library: library.name().to_owned(),
                });
            }
            map.insert(symbol, library);
        }
        Ok(Self {
            interner,
            libraries: map,
        })
    }

    /// Interns a library name into the catalog's own interner, yielding the key
    /// [`ResolvedNativeLibraries::resolve_import`] expects.
    ///
    /// A name already present resolves to its existing symbol, so a declared
    /// library's key always matches the one recorded at build time.
    pub fn intern_library(&mut self, name: &str) -> Result<Symbol, NativeLibraryError> {
        self.interner
            .intern(name)
            .map_err(|_| NativeLibraryError::NameSpaceExhausted)
    }

    /// Resolves an interned library and selected target to its archive path.
    ///
    /// The `library` symbol must come from this catalog's own interner (via
    /// [`ResolvedNativeLibraries::intern_library`]). Returns
    /// [`ImportResolveError::UndeclaredLibrary`] when the catalog has no such
    /// library, and [`ImportResolveError::NoArtifactForTarget`] when it has the
    /// library but no row for exactly `target` — the host-only-library-selected-
    /// for-wasm case.
    pub fn resolve_import(
        &self,
        library: Symbol,
        target: &TargetTriple,
    ) -> Result<&Path, ImportResolveError> {
        let Some(resolved) = self.libraries.get(&library) else {
            return Err(ImportResolveError::UndeclaredLibrary {
                library: self.name_of(library),
            });
        };
        resolved
            .targets()
            .iter()
            .find(|row| row.triple() == target)
            .map(|row| row.archive())
            .ok_or_else(|| ImportResolveError::NoArtifactForTarget {
                library: resolved.name().to_owned(),
                target: target.clone(),
            })
    }

    /// Number of libraries in the catalog.
    pub fn len(&self) -> usize {
        self.libraries.len()
    }

    /// True when the catalog holds no libraries.
    pub fn is_empty(&self) -> bool {
        self.libraries.is_empty()
    }

    /// Renders a symbol as its interned name, or a stable placeholder when the
    /// symbol did not come from this catalog's interner (never panics).
    fn name_of(&self, symbol: Symbol) -> String {
        if (symbol.as_u32() as usize) < self.interner.len() {
            self.interner.resolve(symbol).to_owned()
        } else {
            format!("<symbol {}>", symbol.as_u32())
        }
    }
}

/// Why a foreign import could not be resolved against a catalog and target.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ImportResolveError {
    /// No library of that name is declared for this build.
    #[error(
        "foreign import names native library `{library}`, which is not declared for this build"
    )]
    UndeclaredLibrary {
        /// The undeclared library name.
        library: String,
    },
    /// The library is declared but has no archive for the selected target.
    #[error("native library `{library}` has no native artifact for target `{target}`")]
    NoArtifactForTarget {
        /// The declared library missing an artifact.
        library: String,
        /// The target with no matching row.
        target: TargetTriple,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{NativeLibraryManifest, NativeTargetRow};

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    fn resolved(name: &str, rows: &[(&str, &str)]) -> ResolvedNativeLibrary {
        let targets = rows
            .iter()
            .map(|(t, path)| NativeTargetRow::new(triple(t), *path))
            .collect();
        NativeLibraryManifest::new(name, targets)
            .expect("a valid manifest")
            .resolve(Path::new("/pkg/NativeLibs"), |_| true)
            .expect("resolution")
    }

    fn catalog(libraries: Vec<ResolvedNativeLibrary>) -> ResolvedNativeLibraries {
        ResolvedNativeLibraries::from_resolved(Interner::new(), libraries)
            .expect("a catalog with distinct names")
    }

    #[test]
    fn build_rejects_duplicate_library_names() {
        let error = ResolvedNativeLibraries::from_resolved(
            Interner::new(),
            vec![
                resolved("ffimath", &[("aarch64-macos-none", "a.a")]),
                resolved("ffimath", &[("wasm32-emscripten-unknown", "b.a")]),
            ],
        )
        .expect_err("duplicate library names are rejected");
        assert_eq!(
            error,
            NativeLibraryError::DuplicateLibrary {
                library: "ffimath".to_owned()
            }
        );
    }

    #[test]
    fn resolve_import_returns_the_right_archive_per_target() {
        let mut catalog = catalog(vec![resolved(
            "ffimath",
            &[
                ("aarch64-macos-none", "lib/host.a"),
                ("wasm32-emscripten-unknown", "lib/wasm.a"),
            ],
        )]);
        let symbol = catalog.intern_library("ffimath").expect("interned");
        assert_eq!(
            catalog
                .resolve_import(symbol, &triple("aarch64-macos-none"))
                .expect("host row"),
            Path::new("/pkg/NativeLibs/lib/host.a"),
        );
        assert_eq!(
            catalog
                .resolve_import(symbol, &triple("wasm32-emscripten-unknown"))
                .expect("wasm row"),
            Path::new("/pkg/NativeLibs/lib/wasm.a"),
        );
    }

    #[test]
    fn resolve_import_rejects_an_undeclared_library() {
        let mut catalog = catalog(vec![resolved(
            "ffimath",
            &[("aarch64-macos-none", "lib/host.a")],
        )]);
        let symbol = catalog.intern_library("missing").expect("interned");
        let error = catalog
            .resolve_import(symbol, &triple("aarch64-macos-none"))
            .expect_err("an undeclared library is rejected");
        assert_eq!(
            error,
            ImportResolveError::UndeclaredLibrary {
                library: "missing".to_owned()
            }
        );
        assert!(error.to_string().contains("missing"));
    }

    #[test]
    fn resolve_import_rejects_a_host_only_library_for_wasm() {
        let mut catalog = catalog(vec![resolved(
            "ffimath",
            &[("aarch64-macos-none", "lib/host.a")],
        )]);
        let symbol = catalog.intern_library("ffimath").expect("interned");
        let wasm = triple("wasm32-emscripten-unknown");
        let error = catalog
            .resolve_import(symbol, &wasm)
            .expect_err("a host-only library selected for wasm is rejected");
        assert_eq!(
            error,
            ImportResolveError::NoArtifactForTarget {
                library: "ffimath".to_owned(),
                target: wasm.clone(),
            }
        );
        let message = error.to_string();
        assert!(message.contains("ffimath"), "message names the library");
        assert!(
            message.contains("wasm32-emscripten-unknown"),
            "message names the target",
        );
    }
}
