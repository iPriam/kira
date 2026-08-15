//! A real native library called through the shared libffi engine.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use kira_libffi::{FfiClosure, FfiClosureCallback, LibffiError, LibffiRuntime, PreparedCall};
use kira_runtime_abi::{
    ForeignAggregates, ForeignArg, ForeignCallError, ForeignResult, ForeignSignature,
};

use crate::{DynamicLibrary, FfiError, PROCESS_BINDING_MARKER};

/// Why a native library could not be opened or called through libffi.
#[derive(Debug, thiserror::Error)]
pub enum ForeignLibraryError {
    /// The declared shared library could not be opened.
    #[error("cannot load native library `{path}`: {source}")]
    Library {
        /// The path or loader name from the package declaration.
        path: PathBuf,
        /// The normalized loader error.
        #[source]
        source: FfiError,
    },
    /// Kira's bundled libffi runtime could not be loaded.
    #[error(transparent)]
    Libffi(#[from] LibffiError),
    /// The requested symbol was not exported by the library.
    #[error("native library `{path}` has no symbol `{symbol}`: {source}")]
    Symbol {
        /// The loaded library.
        path: PathBuf,
        /// The requested symbol.
        symbol: String,
        /// The loader error.
        #[source]
        source: FfiError,
    },
    /// The checked foreign call contract failed.
    #[error(transparent)]
    Call(#[from] ForeignCallError),
}

/// One loaded native library and the exact aggregate table used by its calls.
pub struct ForeignLibrary {
    path: PathBuf,
    library: DynamicLibrary,
    process: bool,
    libffi: LibffiRuntime,
    aggregates: ForeignAggregates,
    /// Every import called through this library, by symbol.
    sites: Mutex<HashMap<String, Arc<CallSite>>>,
}

/// One import's resolved address and prepared CIF.
struct CallSite {
    signature: ForeignSignature,
    address: *mut c_void,
    prepared: PreparedCall,
}

// SAFETY: the address is an exported function in a library this value keeps
// loaded, and a `PreparedCall` is itself `Send`/`Sync`. Neither is mutated
// after the site is built.
unsafe impl Send for CallSite {}
// SAFETY: as above — a shared reference grants reads, which is what a call
// through the site performs.
unsafe impl Sync for CallSite {}

impl ForeignLibrary {
    /// Opens `path` and loads Kira's bundled libffi runtime.
    pub fn load(
        path: impl AsRef<Path>,
        aggregates: ForeignAggregates,
    ) -> Result<Self, ForeignLibraryError> {
        Self::load_with_libffi(path, aggregates, LibffiRuntime::load()?)
    }

    /// Opens `path` with a libffi runtime staged at `runtime_path`.
    pub fn load_with_runtime_path(
        path: impl AsRef<Path>,
        aggregates: ForeignAggregates,
        runtime_path: impl AsRef<Path>,
    ) -> Result<Self, ForeignLibraryError> {
        Self::load_with_libffi(path, aggregates, LibffiRuntime::load_from(runtime_path)?)
    }

    /// Opens the current process image and loads Kira's bundled libffi runtime.
    pub fn load_process(aggregates: ForeignAggregates) -> Result<Self, ForeignLibraryError> {
        Self::load_process_with_libffi(aggregates, LibffiRuntime::load()?)
    }

    /// Opens the current process image with a libffi runtime staged at
    /// `runtime_path`.
    pub fn load_process_with_runtime_path(
        aggregates: ForeignAggregates,
        runtime_path: impl AsRef<Path>,
    ) -> Result<Self, ForeignLibraryError> {
        Self::load_process_with_libffi(aggregates, LibffiRuntime::load_from(runtime_path)?)
    }

    fn load_with_libffi(
        path: impl AsRef<Path>,
        aggregates: ForeignAggregates,
        libffi: LibffiRuntime,
    ) -> Result<Self, ForeignLibraryError> {
        let path = path.as_ref().to_path_buf();
        let library =
            DynamicLibrary::open(&path).map_err(|source| ForeignLibraryError::Library {
                path: path.clone(),
                source,
            })?;
        Ok(Self {
            path,
            library,
            process: false,
            libffi,
            aggregates,
            sites: Mutex::new(HashMap::new()),
        })
    }

    fn load_process_with_libffi(
        aggregates: ForeignAggregates,
        libffi: LibffiRuntime,
    ) -> Result<Self, ForeignLibraryError> {
        Ok(Self {
            path: PathBuf::from(PROCESS_BINDING_MARKER),
            library: DynamicLibrary::open_process(),
            process: true,
            libffi,
            aggregates,
            sites: Mutex::new(HashMap::new()),
        })
    }

    /// The path used to open the library.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether this handle resolves symbols from the host process image.
    pub fn is_process(&self) -> bool {
        self.process
    }

    /// Calls `symbol` with libffi and the shared signature/type-layout model.
    ///
    /// # Safety
    /// `signature` must be the exported symbol's real C declaration, and every
    /// pointer `args` carries must stay valid for the duration of the call.
    pub unsafe fn call(
        &self,
        symbol: &str,
        signature: &ForeignSignature,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignLibraryError> {
        let site = self.call_site(symbol, signature)?;
        // SAFETY: the address is the exported function this site resolved, and
        // its CIF was prepared from the caller's own signature.
        unsafe {
            self.libffi.call_with(
                &site.prepared,
                site.address,
                signature,
                &self.aggregates,
                args,
            )
        }
        .map_err(|error| match error {
            LibffiError::Call(call) => ForeignLibraryError::Call(call),
            other => ForeignLibraryError::Libffi(other),
        })
    }

    /// The resolved address and prepared CIF for one import, built once.
    ///
    /// A symbol lookup is a loader call and a CIF is a graph and an
    /// `ffi_prep_cif`; a frame of a graphics program makes thousands of foreign
    /// calls, and neither answer changes between them.
    fn call_site(
        &self,
        symbol: &str,
        signature: &ForeignSignature,
    ) -> Result<Arc<CallSite>, ForeignLibraryError> {
        let mut sites = self.sites.lock().unwrap_or_else(|held| held.into_inner());
        // One symbol carries one declaration in a program, and the signature is
        // checked rather than assumed: a second declaration of the same symbol
        // gets its own site instead of the first one's CIF.
        if let Some(site) = sites
            .get(symbol)
            .filter(|site| &site.signature == signature)
        {
            return Ok(Arc::clone(site));
        }
        // SAFETY: the address is only handed to libffi, whose CIF is prepared
        // from the same signature.
        let address = unsafe { self.library.lookup::<*mut c_void>(symbol) }.map_err(|source| {
            ForeignLibraryError::Symbol {
                path: self.path.clone(),
                symbol: symbol.to_owned(),
                source,
            }
        })?;
        let site = Arc::new(CallSite {
            signature: signature.clone(),
            address: *address,
            prepared: self.libffi.prepare(signature, &self.aggregates)?,
        });
        sites.insert(symbol.to_owned(), Arc::clone(&site));
        Ok(site)
    }

    /// Prepares a libffi closure with the library's bundled runtime.
    ///
    /// The returned closure owns its executable trampoline.
    ///
    /// # Safety
    /// `user_data` must stay valid, and the returned closure must stay alive,
    /// for the complete period in which C may call the address.
    pub unsafe fn closure(
        &self,
        signature: &ForeignSignature,
        callback: FfiClosureCallback,
        user_data: *mut c_void,
    ) -> Result<FfiClosure, ForeignLibraryError> {
        // SAFETY: the caller supplies a callback whose `user_data` remains
        // valid for the closure lifetime; the shared graph is prepared here.
        unsafe {
            FfiClosure::new(
                &self.libffi,
                signature,
                &self.aggregates,
                callback,
                user_data,
            )
        }
        .map_err(ForeignLibraryError::Libffi)
    }

    /// The aggregate table this library's signatures index.
    pub fn aggregates(&self) -> &ForeignAggregates {
        &self.aggregates
    }
}
