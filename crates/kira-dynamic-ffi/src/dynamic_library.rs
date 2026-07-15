//! Cross-platform shared-library handle.
//!
//! Ported from kira-zig
//! `packages/kira_dynamic_ffi/src/dynamic_library.zig`. The Zig version
//! wraps `std.DynLib` (POSIX) / `LoadLibraryExW` (Windows); the Rust port
//! delegates both to `libloading`, which covers the same platforms with the
//! same altered-search-path semantics on Windows.

use thiserror::Error;

/// Errors from library loading and symbol resolution.
/// Zig: the `error.NativeLibraryLoadFailed` / `error.MissingNativeSymbol` /
/// `error.SymbolNameTooLong` set normalized across platforms.
#[derive(Debug, Error)]
pub enum FfiError {
    /// Zig: `error.NativeLibraryLoadFailed`.
    #[error("native library failed to load: {0}")]
    NativeLibraryLoadFailed(#[source] libloading::Error),
    /// Zig: `error.MissingNativeSymbol`.
    #[error("missing native symbol `{name}`")]
    MissingNativeSymbol {
        name: String,
        #[source]
        source: libloading::Error,
    },
    /// Process-image resolution is not scaffolded yet.
    #[error("process-image symbol resolution not yet ported")]
    ProcessLookupUnported,
}

/// A loaded native library, or the current process image.
///
/// Zig: `DynamicLibrary` with `Backend = union(enum) { library, process }`.
/// The `Process` arm resolves symbols from the current process image so
/// statically-linked native libraries (e.g. the in-process `kira_main`
/// developer API) are reachable without a standalone shared object
/// (`dlsym(RTLD_DEFAULT, ..)` / `GetModuleHandleW(null)`).
pub struct DynamicLibrary {
    inner: Backend,
}

enum Backend {
    /// Zig: `.library: NativeLibrary`.
    Library(libloading::Library),
    /// Zig: `.process` — resolve from the current process image.
    /// TODO(port): implement via `libloading::os::unix::Library::this()` /
    /// `libloading::os::windows::Library::this()` at migration.
    Process,
}

impl DynamicLibrary {
    /// Open a shared library at `path`. Zig: `DynamicLibrary.open`
    /// (failures normalized to one error across platforms).
    pub fn open(path: &std::path::Path) -> Result<DynamicLibrary, FfiError> {
        // SAFETY: loading a library runs its platform initializers
        // (constructors, DllMain). As in the Zig runtime, Kira only opens
        // libraries the manifest/user explicitly named; the caller is the
        // trust boundary. No Rust-side invariants are assumed of the loaded
        // code beyond what each later `lookup` asserts.
        let library =
            unsafe { libloading::Library::new(path) }.map_err(FfiError::NativeLibraryLoadFailed)?;
        Ok(DynamicLibrary {
            inner: Backend::Library(library),
        })
    }

    /// Open a handle that resolves symbols from the current process image
    /// instead of a separate shared object. Zig: `DynamicLibrary.openProcess`.
    pub fn open_process() -> DynamicLibrary {
        DynamicLibrary {
            inner: Backend::Process,
        }
    }

    /// Resolve `name` to a symbol of type `T`. Zig: `DynamicLibrary.lookup`.
    ///
    /// # Safety
    /// `T` must accurately describe the symbol's real type (for functions:
    /// exact signature and ABI) — the same unchecked contract as Zig's
    /// `lookup(comptime T, ..)`.
    pub unsafe fn lookup<'lib, T>(
        &'lib self,
        name: &str,
    ) -> Result<libloading::Symbol<'lib, T>, FfiError> {
        match &self.inner {
            Backend::Library(library) => {
                // SAFETY: forwarded caller contract — `T` matches the
                // symbol's real type, per this function's safety docs.
                unsafe { library.get(name.as_bytes()) }.map_err(|source| {
                    FfiError::MissingNativeSymbol {
                        name: name.to_owned(),
                        source,
                    }
                })
            }
            Backend::Process => Err(FfiError::ProcessLookupUnported),
        }
    }

    /// Resolve `name`, returning `None` when absent.
    /// Zig: `DynamicLibrary.lookupOptional`.
    ///
    /// # Safety
    /// Same contract as [`DynamicLibrary::lookup`].
    pub unsafe fn lookup_optional<'lib, T>(
        &'lib self,
        name: &str,
    ) -> Option<libloading::Symbol<'lib, T>> {
        // SAFETY: forwarded caller contract, per this function's safety docs.
        unsafe { self.lookup(name) }.ok()
    }
}

impl std::fmt::Debug for DynamicLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Backend::Library(_) => f.write_str("DynamicLibrary::Library"),
            Backend::Process => f.write_str("DynamicLibrary::Process"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn open_reports_a_missing_library_precisely() {
        let missing = std::path::Path::new("definitely-missing-kira-dynamic-ffi-library");
        assert!(matches!(
            DynamicLibrary::open(missing),
            Err(FfiError::NativeLibraryLoadFailed(_))
        ));
    }
}
