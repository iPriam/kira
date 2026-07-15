//! Cross-platform shared-library handle.
//!
//! Delegates to `libloading`, which wraps `dlopen` (POSIX) and
//! `LoadLibraryExW` (Windows) with altered-search-path semantics.

use thiserror::Error;

/// Errors from library loading and symbol resolution.
#[derive(Debug, Error)]
pub enum FfiError {
    /// A native library failed to load.
    #[error("native library failed to load: {0}")]
    NativeLibraryLoadFailed(#[source] libloading::Error),
    /// A named symbol was not found in the library.
    #[error("missing native symbol `{name}`")]
    MissingNativeSymbol {
        /// The symbol name that could not be resolved.
        name: String,
        #[source]
        source: libloading::Error,
    },
    /// Process-image symbol resolution is not implemented yet.
    #[error("process-image symbol resolution not yet implemented")]
    ProcessLookupUnavailable,
}

/// A loaded native library, or the current process image.
///
/// The `Process` arm resolves symbols from the current process image so
/// statically-linked native code is reachable without a standalone shared
/// object (`dlsym(RTLD_DEFAULT, ..)` / `GetModuleHandleW(null)`).
pub struct DynamicLibrary {
    inner: Backend,
}

enum Backend {
    /// A separately loaded shared library.
    Library(libloading::Library),
    /// Symbols resolved from the current process image.
    // TODO: implement via `libloading::os::unix::Library::this()` /
    // `libloading::os::windows::Library::this()`.
    Process,
}

impl DynamicLibrary {
    /// Open a shared library at `path` (failures normalized across platforms).
    pub fn open(path: &std::path::Path) -> Result<DynamicLibrary, FfiError> {
        // SAFETY: loading a library runs its platform initializers
        // (constructors, DllMain). Kira only opens libraries the manifest or
        // user explicitly named; the caller is the trust boundary. No
        // Rust-side invariants are assumed of the loaded code beyond what each
        // later `lookup` asserts.
        let library =
            unsafe { libloading::Library::new(path) }.map_err(FfiError::NativeLibraryLoadFailed)?;
        Ok(DynamicLibrary {
            inner: Backend::Library(library),
        })
    }

    /// Open a handle that resolves symbols from the current process image
    /// instead of a separate shared object.
    pub fn open_process() -> DynamicLibrary {
        DynamicLibrary {
            inner: Backend::Process,
        }
    }

    /// Resolve `name` to a symbol of type `T`.
    ///
    /// # Safety
    /// `T` must accurately describe the symbol's real type (for functions:
    /// exact signature and ABI).
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
            Backend::Process => Err(FfiError::ProcessLookupUnavailable),
        }
    }

    /// Resolve `name`, returning `None` when absent.
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
