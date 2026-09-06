//! Cross-platform shared-library handle.
//!
//! Delegates to `libloading`, which wraps `dlopen` (POSIX) and `LoadLibraryExW`
//! (Windows).
//!
//! # The library's own directory
//!
//! A library Kira opens may itself depend on shared libraries that ship beside
//! it — a `NativeTarget`'s `runtimeFiles` puts them there, and a hybrid
//! program's native half or a VM adapter sidecar links against them. POSIX finds
//! those through the `RPATH`/`RUNPATH` the link recorded; Windows does not look
//! in the loaded module's directory at all unless it is told to, and searches
//! the *calling process*'s directory instead. So a sidecar sitting beside
//! `webgpu_dawn.dll` failed to load with nothing but `LoadLibraryExW failed`,
//! while the executable built from the same objects — found by the loader in its
//! own directory — started fine.
//!
//! `LOAD_WITH_ALTERED_SEARCH_PATH` is the flag that says "search where this
//! library is". It is honoured only for an absolute path, so the path is made
//! absolute first.
//!
//! # Names that are not paths
//!
//! Not every argument is a path. A driver opened by name — `vulkan-1.dll`,
//! `d3d12.dll`, the host C runtime — is a *module name* the loader resolves
//! through the system search order, and there is no file at that name relative
//! to anything. Making such a name absolute produces a path that does not exist
//! and turns a load that would have succeeded into a failure, so a name that
//! does not resolve to a real file on disk is handed to the loader unchanged.

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
    /// The current process image could not be opened for symbol lookup on
    /// this target.
    #[error("process-image symbol resolution is unavailable on this target")]
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

/// A library handle that is never closed.
///
/// A native library may start a thread — a networking library with an async
/// runtime does it the moment it is first called — and that thread runs the
/// library's own code. Closing the library while one is still in it unmaps the
/// code out from under it, and the thread faults on whichever instruction it
/// reaches next, at a moment with no relation to what the program was doing.
/// POSIX says the same: unloading a library whose code is still executing is
/// undefined.
///
/// Nothing is lost by keeping it. This process never reloads a library it
/// closed, and the toolchain already declines to tear its own address space
/// down on the way out for the same reason — see
/// `kira_toolchain::process::exit`, which says so in as many words.
type HeldLibrary = std::mem::ManuallyDrop<libloading::Library>;

enum Backend {
    /// A separately loaded shared library.
    Library(HeldLibrary),
    /// Symbols resolved from the current process image.
    Process(HeldLibrary),
    /// Process-image lookup is unavailable on the current target.
    ProcessUnavailable,
}

impl DynamicLibrary {
    /// Open a shared library at `path` (failures normalized across platforms).
    pub fn open(path: &std::path::Path) -> Result<DynamicLibrary, FfiError> {
        let library = open_native(path).map_err(FfiError::NativeLibraryLoadFailed)?;
        Ok(DynamicLibrary {
            inner: Backend::Library(std::mem::ManuallyDrop::new(library)),
        })
    }

    /// Open a handle that resolves symbols from the current process image
    /// instead of a separate shared object.
    pub fn open_process() -> DynamicLibrary {
        open_process_handle()
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
            Backend::Process(library) => {
                // SAFETY: forwarded caller contract — `T` matches the
                // symbol's real type, per this function's safety docs.
                unsafe { library.get(name.as_bytes()) }.map_err(|source| {
                    FfiError::MissingNativeSymbol {
                        name: name.to_owned(),
                        source,
                    }
                })
            }
            Backend::ProcessUnavailable => Err(FfiError::ProcessLookupUnavailable),
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

/// Opens a shared library the way every Kira host should, and hands back the
/// raw `libloading` handle.
///
/// Public because the hybrid host loads its native half itself — it deliberately
/// resolves every `kira_rt_*` symbol out of that library rather than out of this
/// process — and the *search* rule is the same question there as here.
///
/// # Safety
/// Loading a library runs its platform initializers (constructors, `DllMain`).
/// Kira only opens libraries the manifest or the user explicitly named; the
/// caller is the trust boundary.
pub fn open_shared_library(
    path: &std::path::Path,
) -> Result<libloading::Library, libloading::Error> {
    open_native(path)
}

/// Loads the library, telling Windows to look beside it for what it depends on.
///
/// # Safety
/// See [`open_shared_library`].
#[cfg(windows)]
fn open_native(path: &std::path::Path) -> Result<libloading::Library, libloading::Error> {
    use libloading::os::windows::{LOAD_WITH_ALTERED_SEARCH_PATH, Library};

    // The flag is honoured only for an absolute path, and only a path that
    // names a real file can be made absolute meaningfully; a module name goes
    // to the loader as it was written so the system search order applies.
    let library = match module_file(path) {
        // SAFETY: see this function's own safety note.
        Some(file) => unsafe { Library::load_with_flags(file, LOAD_WITH_ALTERED_SEARCH_PATH) }?,
        // SAFETY: see this function's own safety note.
        None => unsafe { Library::new(path) }?,
    };
    Ok(library.into())
}

/// Opens the current process image as a shared-library handle.
///
/// Public because an embedded application's native half *is* this process:
/// its hybrid manifest records [`SELF_LIBRARY_MARKER`] instead of a path, and
/// the hybrid host binds every trampoline and helper out of the running
/// image. The same export rule applies as for any library opened this way —
/// symbols only resolve if the image exports them.
///
/// # Safety
/// Opening the process image runs no new initializers and loads no code; the
/// returned handle aliases what is already executing. Callers keep it alive
/// only as long as the process itself.
pub fn open_process_image() -> Result<libloading::Library, FfiError> {
    process_image()
}

/// Gets a handle for the current process image without loading a new module.
#[cfg(unix)]
fn process_image() -> Result<libloading::Library, FfiError> {
    Ok(libloading::os::unix::Library::this().into())
}

/// Gets a handle for the executable that launched the current process.
#[cfg(windows)]
fn process_image() -> Result<libloading::Library, FfiError> {
    libloading::os::windows::Library::this()
        .map(Into::into)
        .map_err(|_| FfiError::ProcessLookupUnavailable)
}

/// Process-image lookup is not defined by `libloading` on other targets.
#[cfg(not(any(unix, windows)))]
fn process_image() -> Result<libloading::Library, FfiError> {
    Err(FfiError::ProcessLookupUnavailable)
}

/// Opens a handle that resolves symbols from the current process image
/// instead of a separate shared object.
fn open_process_handle() -> DynamicLibrary {
    let inner = match process_image() {
        Ok(library) => Backend::Process(std::mem::ManuallyDrop::new(library)),
        Err(_) => Backend::ProcessUnavailable,
    };
    DynamicLibrary { inner }
}

/// The absolute path of the file `path` names, or `None` when it names no file
/// — which is how a module name resolved through the system search order, and a
/// library that is simply absent, both present.
#[cfg(windows)]
fn module_file(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let absolute = std::path::absolute(path).ok()?;
    absolute.is_file().then_some(absolute)
}

/// The POSIX side: `dlopen` already honours the `RUNPATH` the link recorded, so
/// there is nothing to add.
///
/// # Safety
/// See [`open_shared_library`].
#[cfg(not(windows))]
fn open_native(path: &std::path::Path) -> Result<libloading::Library, libloading::Error> {
    // SAFETY: see this function's own safety note.
    unsafe { libloading::Library::new(path) }
}

impl std::fmt::Debug for DynamicLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.inner {
            Backend::Library(_) => f.write_str("DynamicLibrary::Library"),
            Backend::Process(_) => f.write_str("DynamicLibrary::Process"),
            Backend::ProcessUnavailable => f.write_str("DynamicLibrary::ProcessUnavailable"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The C library under whatever name this platform gives it — a name the
    /// loader resolves through its own search order, with no file at that name
    /// relative to this process.
    const HOST_C_LIBRARY: &str = if cfg!(windows) {
        "msvcrt.dll"
    } else if cfg!(target_vendor = "apple") {
        "libSystem.B.dylib"
    } else {
        "libc.so.6"
    };

    #[test]
    fn a_bare_module_name_still_reaches_the_system_search_order() {
        assert!(
            DynamicLibrary::open(std::path::Path::new(HOST_C_LIBRARY)).is_ok(),
            "a name that is not a path must not be resolved against the cwd"
        );
    }

    #[test]
    fn open_reports_a_missing_library_precisely() {
        let missing = std::path::Path::new("definitely-missing-kira-dynamic-ffi-library");
        assert!(matches!(
            DynamicLibrary::open(missing),
            Err(FfiError::NativeLibraryLoadFailed(_))
        ));
    }

    #[test]
    fn process_image_lookup_is_available_and_reports_missing_symbols() {
        let process = DynamicLibrary::open_process();
        // SAFETY: The symbol is intentionally absent, so the test never calls
        // the function pointer and only checks the lookup error path.
        let result =
            unsafe { process.lookup::<unsafe extern "C" fn()>("kira_definitely_no_such_symbol") };

        assert!(matches!(
            result,
            Err(FfiError::MissingNativeSymbol { name, .. }) if name == "kira_definitely_no_such_symbol"
        ));
        // SAFETY: As above, the optional lookup is only expected to return no
        // symbol and the returned function pointer is never invoked.
        let optional = unsafe {
            process.lookup_optional::<unsafe extern "C" fn()>("kira_definitely_no_such_symbol")
        };
        assert!(optional.is_none());
    }

    #[cfg(unix)]
    #[test]
    fn process_image_resolves_a_symbol_from_the_loaded_process() {
        let process = DynamicLibrary::open_process();
        // SAFETY: The test only checks that the platform handle resolves the
        // standard allocator; it never calls or stores the function pointer.
        let malloc = unsafe {
            process.lookup::<unsafe extern "C" fn(usize) -> *mut std::ffi::c_void>("malloc")
        };
        assert!(malloc.is_ok());
    }
}
