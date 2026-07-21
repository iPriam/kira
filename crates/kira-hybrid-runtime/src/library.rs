//! The loaded native half: `dlopen`, symbol binding, and the string helpers the
//! host reaches the library's heap through.
//!
//! # Why nothing here links against the runtime
//!
//! This crate deliberately does not depend on `kira-native-bridge`. The process
//! hosting a hybrid program almost certainly carries its own copy of the
//! `kira_rt_*` symbols already — `kirac` links the runtime as an rlib so cargo
//! also builds the staticlib the compiler links — and a string handle allocated
//! by one copy and freed by the other is a cross-allocator free.
//!
//! So every symbol is resolved out of the *loaded library* by name.
//! `libloading` opens with `RTLD_LOCAL`, so `dlsym` on the returned handle
//! searches that library rather than the global namespace, which is what keeps
//! the two copies apart. Never resolve one of these by calling it directly.

use std::ffi::c_void;
use std::path::{Path, PathBuf};

use kira_hybrid_definition::{HybridForeign, HybridFunction};
use kira_runtime_abi::{BridgeValue, FOREIGN_ADAPTER_ABI_MARKER, ForeignAdapterFn};

use crate::error::HybridError;

/// A native function's trampoline: `kira_native_fn_<id>`.
///
/// Every native function is reached through one of these regardless of its own
/// signature, so the host marshals into one shape instead of building a call
/// per signature.
///
/// # Safety
/// `args` must point at `count` readable [`BridgeValue`]s (or be null when
/// `count` is 0), and `out` must point at one writable [`BridgeValue`].
pub type TrampolineFn =
    unsafe extern "C" fn(args: *const BridgeValue, count: u32, out: *mut BridgeValue);

/// The host callback the library calls to run a `@Runtime` function.
///
/// Mirrors `kira_hybrid_call_runtime`'s invoker type in `kira-native-bridge`.
/// It is spelled out again here rather than imported for the reason in this
/// module's docs: the host must not link the runtime.
///
/// # Safety
/// The same contract as [`TrampolineFn`], plus a `function_id` naming a
/// `@Runtime` function.
pub type RuntimeInvoker = unsafe extern "C" fn(
    function_id: u32,
    args: *const BridgeValue,
    count: u32,
    out: *mut BridgeValue,
);

/// An opaque `KStr` handle into the loaded library's string heap.
///
/// A `u64` rather than a newtype because that is the contract's own spelling:
/// [`kira_runtime_abi::BridgeData::String`] carries the handle as a `u64`, and
/// this is the same handle. It is opaque here — this crate never dereferences
/// one, and could not: `KiraString` is private to the runtime.
pub type StrHandle = u64;

type StrNewFn = unsafe extern "C" fn(data: *const u8, len: usize) -> *mut c_void;
type StrFreeFn = unsafe extern "C" fn(value: *mut c_void);
type StrDataFn = unsafe extern "C" fn(value: *mut c_void) -> *const u8;
type StrLenFn = unsafe extern "C" fn(value: *mut c_void) -> usize;
type InstallInvokerFn = unsafe extern "C" fn(invoker: Option<RuntimeInvoker>);
/// The versioned foreign-adapter marker; resolving it proves the native half
/// carries this build's adapter ABI, exactly as [`FOREIGN_ADAPTER_ABI_MARKER`]
/// names. A no-argument function whose body is irrelevant — it is never called.
type ForeignMarkerFn = unsafe extern "C" fn();

/// The symbols every hybrid library must export, whatever the program does.
const STR_NEW: &[u8] = b"kira_rt_str_new\0";
const STR_FREE: &[u8] = b"kira_rt_str_free\0";
const STR_DATA: &[u8] = b"kira_rt_str_data\0";
const STR_LEN: &[u8] = b"kira_rt_str_len\0";
const INSTALL_INVOKER: &[u8] = b"kira_hybrid_install_runtime_invoker\0";

/// The native half of a hybrid program, loaded and bound.
pub struct NativeLibrary {
    /// Where the library was loaded from, for diagnostics.
    path: PathBuf,
    /// Each function's trampoline by function id; `None` for a runtime function.
    ///
    /// Indexed by id rather than searched, so reaching a trampoline is a total
    /// function of the id the VM hands over.
    trampolines: Vec<Option<TrampolineFn>>,
    /// Each foreign import's generated adapter by import id.
    ///
    /// Bound out of this same library — never a second `dlopen` of the C library
    /// or a separate sidecar — so a runtime-half foreign call and a native-half
    /// one reach one copy of the C code. Indexed by import id, so reaching an
    /// adapter is a total function of the id the bytecode's `CallForeign` names.
    adapters: Vec<ForeignAdapterFn>,
    str_new: StrNewFn,
    str_free: StrFreeFn,
    str_data: StrDataFn,
    str_len: StrLenFn,
    install_invoker: InstallInvokerFn,
    /// The open library. Declared last so it is dropped last: every function
    /// pointer above points into its image and dangles once it is unloaded.
    _library: libloading::Library,
}

impl NativeLibrary {
    /// Loads `path` and binds every symbol the host needs: the string helpers,
    /// the runtime invoker, one trampoline per native function in `functions`,
    /// and — when `foreign` is non-empty — the foreign-adapter marker and one
    /// adapter per import.
    pub fn load(
        path: &Path,
        functions: &[HybridFunction],
        foreign: &[HybridForeign],
    ) -> Result<NativeLibrary, HybridError> {
        // SAFETY: loading a library runs its initializers, which is why this is
        // unsafe. The library is one this toolchain built and named in a
        // manifest it also wrote; a host that cannot trust its own build has
        // already lost.
        let library =
            unsafe { libloading::Library::new(path) }.map_err(|source| HybridError::Library {
                path: path.to_path_buf(),
                source,
            })?;

        let str_new = bind(&library, path, STR_NEW)?;
        let str_free = bind(&library, path, STR_FREE)?;
        let str_data = bind(&library, path, STR_DATA)?;
        let str_len = bind(&library, path, STR_LEN)?;
        let install_invoker = bind(&library, path, INSTALL_INVOKER)?;

        let mut trampolines = vec![None; functions.len()];
        for function in functions {
            let Some(symbol) = &function.exported_name else {
                continue;
            };
            let mut name = symbol.clone().into_bytes();
            name.push(0);
            let trampoline: TrampolineFn = bind(&library, path, &name)?;
            let slot = trampolines.get_mut(function.id as usize).ok_or_else(|| {
                HybridError::Mismatch(format!(
                    "function `{}` has id {} but the manifest carries only {} functions",
                    function.name,
                    function.id,
                    functions.len(),
                ))
            })?;
            *slot = Some(trampoline);
        }

        // Foreign adapters live in this same library. Prove its adapter ABI
        // before binding any: a marker that will not resolve means a stale or
        // incompatible native half, caught here by name rather than by a wrong
        // answer at the first foreign call.
        let mut adapters = Vec::with_capacity(foreign.len());
        if !foreign.is_empty() {
            let mut marker = FOREIGN_ADAPTER_ABI_MARKER.as_bytes().to_vec();
            marker.push(0);
            let _: ForeignMarkerFn = bind(&library, path, &marker)?;
            for import in foreign {
                let mut name = import.adapter_symbol.clone().into_bytes();
                name.push(0);
                adapters.push(bind::<ForeignAdapterFn>(&library, path, &name)?);
            }
        }

        Ok(NativeLibrary {
            path: path.to_path_buf(),
            trampolines,
            adapters,
            str_new,
            str_free,
            str_data,
            str_len,
            install_invoker,
            _library: library,
        })
    }

    /// Where this library was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The trampoline for `function_id`, or `None` when it names no native
    /// function.
    pub fn trampoline(&self, function_id: u32) -> Option<TrampolineFn> {
        self.trampolines
            .get(function_id as usize)
            .copied()
            .flatten()
    }

    /// The generated adapter for foreign import `foreign_id`, or `None` when the
    /// id names no import this library bound.
    pub fn adapter(&self, foreign_id: u32) -> Option<ForeignAdapterFn> {
        self.adapters.get(foreign_id as usize).copied()
    }

    /// Installs the host's runtime invoker, or clears it with `None`.
    ///
    /// # Safety
    /// `invoker` must stay callable until it is cleared or the process exits.
    pub unsafe fn install_invoker(&self, invoker: Option<RuntimeInvoker>) {
        // SAFETY: forwarding the caller's own guarantee to the library's
        // installer, whose contract is the same one.
        unsafe { (self.install_invoker)(invoker) };
    }

    /// Calls `trampoline` with `args`, returning what it wrote.
    ///
    /// # Safety
    /// `trampoline` must be one of this library's, and `args` must match the
    /// signature the manifest records for it — the callee is machine code that
    /// reads them by position and cannot check.
    pub unsafe fn call(&self, trampoline: TrampolineFn, args: &[BridgeValue]) -> BridgeValue {
        let mut out = BridgeValue::VOID;
        let count = args.len() as u32;
        // A zero-length `Vec`'s pointer is dangling-but-aligned rather than
        // null; the ABI permits null when the count is 0, so say null.
        let pointer = if args.is_empty() {
            std::ptr::null()
        } else {
            args.as_ptr()
        };
        // SAFETY: `pointer` covers `count` readable values (or is null when
        // there are none) and `out` is one writable value on this stack frame.
        // The caller vouches for the signature match.
        unsafe { trampoline(pointer, count, &mut out) };
        out
    }

    /// Copies `text` into a fresh handle from the library's own allocator.
    pub fn new_string(&self, text: &str) -> StrHandle {
        // SAFETY: the slice covers exactly `len` readable bytes.
        let handle = unsafe { (self.str_new)(text.as_ptr(), text.len()) };
        handle as StrHandle
    }

    /// Copies a handle's bytes out, leaving the handle live.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library.
    pub unsafe fn read_string(&self, handle: StrHandle) -> Result<String, std::str::Utf8Error> {
        let pointer = handle as *mut c_void;
        // SAFETY: the caller vouches the handle is live (or null); both helpers
        // accept null as the empty string.
        let (data, len) = unsafe { ((self.str_data)(pointer), (self.str_len)(pointer)) };
        if len == 0 {
            return Ok(String::new());
        }
        // SAFETY: `kira_rt_str_data` returns a pointer valid for
        // `kira_rt_str_len` bytes for as long as the handle is unfreed, and
        // nothing frees it between those two calls and this read.
        let bytes = unsafe { std::slice::from_raw_parts(data, len) };
        // Validated rather than trusted: the bytes crossed an ABI, and a wrong
        // answer is worse than a rejection.
        std::str::from_utf8(bytes).map(str::to_owned)
    }

    /// Frees a handle. A null handle is a no-op.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library, freed once.
    pub unsafe fn free_string(&self, handle: StrHandle) {
        // SAFETY: the caller upholds the free-once contract.
        unsafe { (self.str_free)(handle as *mut c_void) };
    }

    /// Copies a handle's bytes out and frees it: the host taking ownership.
    ///
    /// # Safety
    /// `handle` must be null or a live handle from this library, freed once —
    /// which this does.
    pub unsafe fn take_string(&self, handle: StrHandle) -> Result<String, std::str::Utf8Error> {
        // SAFETY: the caller vouches the handle is live; read before free.
        let text = unsafe { self.read_string(handle) };
        // SAFETY: the same handle, consumed exactly once here, and not read
        // again — the bytes were copied out above, including on the error path.
        unsafe { self.free_string(handle) };
        text
    }
}

/// Resolves one symbol out of `library`, naming it when it is absent.
fn bind<T: Copy>(
    library: &libloading::Library,
    path: &Path,
    symbol: &[u8],
) -> Result<T, HybridError> {
    // SAFETY: `T` is the signature this crate declares for `symbol`, and the
    // library is one this toolchain built against the same declarations. The
    // returned value is copied out of the `Symbol` and stays valid while the
    // library is loaded, which `NativeLibrary` guarantees by owning it.
    let resolved =
        unsafe { library.get::<T>(symbol) }.map_err(|source| HybridError::MissingSymbol {
            path: path.to_path_buf(),
            symbol: String::from_utf8_lossy(symbol.strip_suffix(b"\0").unwrap_or(symbol))
                .into_owned(),
            source,
        })?;
    Ok(*resolved)
}
