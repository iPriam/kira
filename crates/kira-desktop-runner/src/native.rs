//! Loading and invoking a whole-program native live library.

use std::path::Path;

use kira_dynamic_ffi::{DynamicLibrary, FfiError};
use kira_runtime_abi::NATIVE_LIVE_ENTRY_SYMBOL;

type NativeEntry = unsafe extern "C" fn() -> i32;

/// Why a whole-program native live entry could not be loaded or completed.
#[derive(Debug, thiserror::Error)]
pub enum NativeProgramError {
    /// The staged shared library or its entry symbol was unavailable.
    #[error(transparent)]
    Library(#[from] FfiError),
    /// The native entry returned a failure status.
    #[error("native live entry returned status {code}")]
    Exit {
        /// The status returned by the native entry.
        code: i32,
    },
}

/// A loaded whole-program native live library and its fixed entrypoint.
pub struct NativeProgram {
    _library: DynamicLibrary,
    entry: NativeEntry,
}

impl NativeProgram {
    /// Opens `path` and binds the whole-program live entry symbol.
    pub fn load(path: &Path) -> Result<Self, NativeProgramError> {
        let library = DynamicLibrary::open(path)?;
        let entry = {
            // SAFETY: the LLVM live backend emits this exact symbol with the
            // exact C ABI and signature; the library remains owned by the
            // returned program for as long as the copied function pointer runs.
            unsafe { *library.lookup::<NativeEntry>(NATIVE_LIVE_ENTRY_SYMBOL)? }
        };
        Ok(Self {
            _library: library,
            entry,
        })
    }

    /// Runs the native entry in the desktop runner process.
    pub fn run(&self) -> Result<(), NativeProgramError> {
        // SAFETY: `entry` was resolved from the library using the backend's
        // fixed `unsafe extern "C" fn() -> i32` contract, and `library` keeps
        // the code mapped for the duration of this call.
        let code = unsafe { (self.entry)() };
        if code == 0 {
            Ok(())
        } else {
            Err(NativeProgramError::Exit { code })
        }
    }
}

impl std::fmt::Debug for NativeProgram {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NativeProgram")
    }
}
