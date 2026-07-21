//! Typed loading and safe calling of Kira-generated foreign adapters.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};

use kira_runtime_abi::{
    BridgeData, BridgeValue, FOREIGN_ADAPTER_ABI_MARKER, FOREIGN_STRING_DATA_SYMBOL,
    FOREIGN_STRING_FREE_SYMBOL, FOREIGN_STRING_LEN_SYMBOL, FOREIGN_STRING_NEW_SYMBOL,
    ForeignAdapterFn, ForeignAdapterStatus, ForeignArg, ForeignCallError, ForeignResult,
    ForeignSignature, ForeignType,
};
use thiserror::Error;

use crate::{DynamicLibrary, FfiError};

type MarkerFn = unsafe extern "C" fn();
type StrNewFn = unsafe extern "C" fn(data: *const u8, len: usize) -> *mut c_void;
type StrFreeFn = unsafe extern "C" fn(value: *mut c_void);
type StrDataFn = unsafe extern "C" fn(value: *mut c_void) -> *const u8;
type StrLenFn = unsafe extern "C" fn(value: *mut c_void) -> usize;

/// Why a generated foreign-adapter library could not be loaded or called.
#[derive(Debug, Error)]
pub enum ForeignAdapterError {
    /// The shared library itself could not be opened.
    #[error("cannot load foreign adapter library `{path}`: {source}")]
    Library {
        /// The selected build artifact.
        path: PathBuf,
        /// The platform loader failure.
        #[source]
        source: FfiError,
    },
    /// The library does not export this runtime's versioned adapter marker.
    #[error(
        "foreign adapter library `{path}` is stale or incompatible: missing ABI marker `{marker}`"
    )]
    IncompatibleAbi {
        /// The selected build artifact.
        path: PathBuf,
        /// The required marker symbol.
        marker: &'static str,
        /// The platform symbol-resolution failure.
        #[source]
        source: FfiError,
    },
    /// A required string helper is absent from the adapter library.
    #[error("foreign adapter library `{path}` is missing string helper `{symbol}`")]
    MissingStringHelper {
        /// The selected build artifact.
        path: PathBuf,
        /// The absent helper symbol.
        symbol: &'static str,
        /// The platform symbol-resolution failure.
        #[source]
        source: FfiError,
    },
    /// An adapter symbol is absent from the loaded library.
    #[error("foreign adapter library `{path}` is missing adapter `{symbol}`")]
    MissingAdapter {
        /// The selected build artifact.
        path: PathBuf,
        /// The absent adapter symbol.
        symbol: String,
        /// The platform symbol-resolution failure.
        #[source]
        source: FfiError,
    },
    /// A symbol name contains an interior NUL and cannot be passed to the loader.
    #[error("foreign adapter symbol `{symbol}` contains an interior NUL byte")]
    MalformedSymbol {
        /// The rejected symbol name.
        symbol: String,
    },
    /// The safe call contract was violated or the adapter returned malformed data.
    #[error(transparent)]
    Call(#[from] ForeignCallError),
}

struct StringHelpers {
    new: StrNewFn,
    free: StrFreeFn,
    _data: StrDataFn,
    _len: StrLenFn,
}

/// A loaded, ABI-checked Kira-generated foreign-adapter library.
///
/// Adapter entrypoints are resolved once and cached by symbol. The library owns
/// all copied function pointers and is declared last so it unloads after them.
pub struct ForeignAdapterLibrary {
    path: PathBuf,
    adapters: RefCell<HashMap<String, ForeignAdapterFn>>,
    strings: StringHelpers,
    library: DynamicLibrary,
}

impl ForeignAdapterLibrary {
    /// Loads a generated adapter library and binds its version marker and string helpers.
    pub fn load(path: &Path) -> Result<Self, ForeignAdapterError> {
        let library =
            DynamicLibrary::open(path).map_err(|source| ForeignAdapterError::Library {
                path: path.to_path_buf(),
                source,
            })?;

        bind_marker(&library).map_err(|source| ForeignAdapterError::IncompatibleAbi {
            path: path.to_path_buf(),
            marker: FOREIGN_ADAPTER_ABI_MARKER,
            source,
        })?;

        let strings = StringHelpers {
            new: bind_helper(&library, path, FOREIGN_STRING_NEW_SYMBOL)?,
            free: bind_helper(&library, path, FOREIGN_STRING_FREE_SYMBOL)?,
            _data: bind_helper(&library, path, FOREIGN_STRING_DATA_SYMBOL)?,
            _len: bind_helper(&library, path, FOREIGN_STRING_LEN_SYMBOL)?,
        };

        Ok(Self {
            path: path.to_path_buf(),
            adapters: RefCell::new(HashMap::new()),
            strings,
            library,
        })
    }

    /// Returns the artifact this loader opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Calls one generated adapter using its declared exact-width signature.
    ///
    /// `adapter_symbol` is build-produced metadata, not a function pointer or a
    /// bytecode-provided path. The first call resolves it; later calls reuse the
    /// cached typed entrypoint.
    pub fn call(
        &self,
        adapter_symbol: &str,
        signature: &ForeignSignature,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignAdapterError> {
        let adapter = self.adapter(adapter_symbol)?;
        call_adapter(adapter, signature, args, &self.strings).map_err(Into::into)
    }

    fn adapter(&self, symbol: &str) -> Result<ForeignAdapterFn, ForeignAdapterError> {
        validate_symbol(symbol)?;
        if let Some(adapter) = self.adapters.borrow().get(symbol).copied() {
            return Ok(adapter);
        }

        // SAFETY: generated adapter symbols all have `ForeignAdapterFn`'s exact
        // versioned signature. The marker and helpers were verified at load,
        // and the copied pointer remains valid while `self.library` is alive.
        let adapter = unsafe { self.library.lookup::<ForeignAdapterFn>(symbol) }
            .map(|resolved| *resolved)
            .map_err(|source| ForeignAdapterError::MissingAdapter {
                path: self.path.clone(),
                symbol: symbol.to_owned(),
                source,
            })?;
        self.adapters
            .borrow_mut()
            .insert(symbol.to_owned(), adapter);
        Ok(adapter)
    }
}

impl std::fmt::Debug for ForeignAdapterLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForeignAdapterLibrary")
            .field("path", &self.path)
            .field("bound_adapters", &self.adapters.borrow().len())
            .finish_non_exhaustive()
    }
}

fn bind_marker(library: &DynamicLibrary) -> Result<MarkerFn, FfiError> {
    // SAFETY: the marker is a no-argument C function whose body is irrelevant;
    // resolving the versioned name is the compatibility check. It is not called.
    unsafe { library.lookup::<MarkerFn>(FOREIGN_ADAPTER_ABI_MARKER) }.map(|marker| *marker)
}

fn bind_helper<T: Copy>(
    library: &DynamicLibrary,
    path: &Path,
    symbol: &'static str,
) -> Result<T, ForeignAdapterError> {
    // SAFETY: each call site chooses `T` from the fixed version-1 helper
    // contract for this exact symbol. The marker was checked first.
    unsafe { library.lookup::<T>(symbol) }
        .map(|resolved| *resolved)
        .map_err(|source| ForeignAdapterError::MissingStringHelper {
            path: path.to_path_buf(),
            symbol,
            source,
        })
}

fn validate_symbol(symbol: &str) -> Result<(), ForeignAdapterError> {
    if symbol.as_bytes().contains(&0) {
        return Err(ForeignAdapterError::MalformedSymbol {
            symbol: symbol.to_owned(),
        });
    }
    Ok(())
}

fn call_adapter(
    adapter: ForeignAdapterFn,
    signature: &ForeignSignature,
    args: &[ForeignArg<'_>],
    helpers: &StringHelpers,
) -> Result<ForeignResult, ForeignCallError> {
    if signature.result() == ForeignType::CString {
        return Err(ForeignCallError::UnsupportedResultType(
            ForeignType::CString,
        ));
    }
    if args.len() != signature.parameters().len() {
        return Err(ForeignCallError::ArgumentCount {
            expected: signature.parameters().len(),
            actual: args.len(),
        });
    }
    let count = u32::try_from(args.len())
        .map_err(|_| ForeignCallError::TooManyArguments { actual: args.len() })?;

    let mut temporary_strings = TemporaryStrings::new(helpers);
    let mut lowered = Vec::with_capacity(args.len());
    for (index, (expected, argument)) in signature
        .parameters()
        .iter()
        .copied()
        .zip(args.iter().copied())
        .enumerate()
    {
        lowered.push(lower_argument(
            expected,
            argument,
            index,
            &mut temporary_strings,
        )?);
    }

    let mut out = BridgeValue::VOID;
    let pointer = if lowered.is_empty() {
        std::ptr::null()
    } else {
        lowered.as_ptr()
    };
    // SAFETY: `pointer` covers exactly `count` initialized bridge values (or is
    // null for zero), `out` is writable for this call, and `adapter` was bound
    // only after the version-1 marker was verified.
    let status = unsafe { adapter(pointer, count, &mut out) };
    drop(temporary_strings);

    match status {
        ForeignAdapterStatus::SUCCESS => lift_result(signature.result(), out),
        ForeignAdapterStatus::BAD_ARGUMENT_COUNT => Err(ForeignCallError::AdapterBadArgumentCount),
        ForeignAdapterStatus::BAD_ARGUMENT_TAG => Err(ForeignCallError::AdapterBadArgumentTag),
        ForeignAdapterStatus::INTERIOR_NUL => Err(ForeignCallError::AdapterInteriorNul),
        ForeignAdapterStatus::MALFORMED_RESULT => Err(ForeignCallError::AdapterMalformedResult),
        ForeignAdapterStatus(status) => Err(ForeignCallError::UnknownAdapterStatus(status)),
    }
}

fn lower_argument(
    expected: ForeignType,
    argument: ForeignArg<'_>,
    index: usize,
    strings: &mut TemporaryStrings<'_>,
) -> Result<BridgeValue, ForeignCallError> {
    let actual = argument.foreign_type();
    if actual != expected {
        return Err(ForeignCallError::ArgumentType {
            index,
            expected,
            actual,
        });
    }

    let value = match argument {
        ForeignArg::Void => BridgeData::Void,
        ForeignArg::I8(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::I16(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::I32(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::I64(value) => BridgeData::Int(value),
        ForeignArg::U8(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::U16(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::U32(value) => BridgeData::Int(i64::from(value)),
        ForeignArg::U64(value) => BridgeData::Int(value as i64),
        ForeignArg::Bool(value) => BridgeData::Bool(value),
        ForeignArg::F32(value) => BridgeData::Float(f64::from(value)),
        ForeignArg::F64(value) => BridgeData::Float(value),
        ForeignArg::RawPtr(value) => {
            check_pointer_width(value)?;
            BridgeData::RawPtr(value)
        }
        ForeignArg::CString(text) => {
            if text.as_bytes().contains(&0) {
                return Err(ForeignCallError::InteriorNul { index });
            }
            BridgeData::String(strings.copy(text))
        }
    };
    Ok(BridgeValue::encode(value))
}

fn lift_result(
    expected: ForeignType,
    value: BridgeValue,
) -> Result<ForeignResult, ForeignCallError> {
    if value.reserved != [0; 7] {
        return Err(ForeignCallError::MalformedResultReserved);
    }
    if value.tag != expected.bridge_tag() {
        return Err(ForeignCallError::MalformedResultTag {
            expected: expected.bridge_tag().0,
            actual: value.tag.0,
        });
    }

    let result = match expected {
        ForeignType::Void => ForeignResult::Void,
        ForeignType::I8 => ForeignResult::I8(value.payload as i8),
        ForeignType::I16 => ForeignResult::I16(value.payload as i16),
        ForeignType::I32 => ForeignResult::I32(value.payload as i32),
        ForeignType::I64 => ForeignResult::I64(value.payload as i64),
        ForeignType::U8 => ForeignResult::U8(value.payload as u8),
        ForeignType::U16 => ForeignResult::U16(value.payload as u16),
        ForeignType::U32 => ForeignResult::U32(value.payload as u32),
        ForeignType::U64 => ForeignResult::U64(value.payload),
        ForeignType::Bool => ForeignResult::Bool(value.payload != 0),
        ForeignType::F32 => ForeignResult::F32(f64::from_bits(value.payload) as f32),
        ForeignType::F64 => ForeignResult::F64(f64::from_bits(value.payload)),
        ForeignType::RawPtr => {
            check_pointer_width(value.payload)?;
            ForeignResult::RawPtr(value.payload)
        }
        ForeignType::CString => {
            return Err(ForeignCallError::UnsupportedResultType(
                ForeignType::CString,
            ));
        }
    };
    Ok(result)
}

fn check_pointer_width(value: u64) -> Result<(), ForeignCallError> {
    if (value as usize) as u64 != value {
        return Err(ForeignCallError::RawPointerOutOfRange { value });
    }
    Ok(())
}

struct TemporaryStrings<'a> {
    helpers: &'a StringHelpers,
    handles: Vec<*mut c_void>,
}

impl<'a> TemporaryStrings<'a> {
    fn new(helpers: &'a StringHelpers) -> Self {
        Self {
            helpers,
            handles: Vec::new(),
        }
    }

    fn copy(&mut self, text: &str) -> u64 {
        // SAFETY: `text` covers exactly `len` readable bytes for this call. The
        // helper copies them into storage owned by this adapter library.
        let handle = unsafe { (self.helpers.new)(text.as_ptr(), text.len()) };
        self.handles.push(handle);
        handle as u64
    }
}

impl Drop for TemporaryStrings<'_> {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            // SAFETY: every handle was returned by this library's `str_new`, is
            // still live, and is freed exactly once here on every exit path.
            unsafe { (self.helpers.free)(handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;

    static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

    const ADAPTER_SOURCE: &str = r#"
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct BridgeValue { tag: u8, reserved: [u8; 7], payload: u64 }

#[unsafe(no_mangle)]
pub extern "C" fn kira_foreign_adapter_abi_version_1() {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_new(data: *const u8, len: usize) -> *mut c_void {
    let bytes = if len == 0 { Vec::new() } else {
        unsafe { std::slice::from_raw_parts(data, len) }.to_vec()
    };
    Box::into_raw(Box::new(bytes)).cast()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_free(value: *mut c_void) {
    if !value.is_null() { unsafe { drop(Box::from_raw(value.cast::<Vec<u8>>())) }; }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_data(value: *mut c_void) -> *const u8 {
    if value.is_null() { std::ptr::null() } else { unsafe { (&*value.cast::<Vec<u8>>()).as_ptr() } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_str_len(value: *mut c_void) -> usize {
    if value.is_null() { 0 } else { unsafe { (&*value.cast::<Vec<u8>>()).len() } }
}

unsafe fn args<'a>(data: *const BridgeValue, count: u32) -> &'a [BridgeValue] {
    if count == 0 { &[] } else { unsafe { std::slice::from_raw_parts(data, count as usize) } }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_add(data: *const BridgeValue, count: u32, out: *mut BridgeValue) -> u32 {
    if count != 2 { return 1; }
    let values = unsafe { args(data, count) };
    if values[0].tag != 1 || values[1].tag != 1 { return 2; }
    unsafe { *out = BridgeValue { tag: 1, reserved: [0; 7], payload: values[0].payload.wrapping_add(values[1].payload) }; }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_cstring_len(data: *const BridgeValue, count: u32, out: *mut BridgeValue) -> u32 {
    if count != 1 { return 1; }
    let values = unsafe { args(data, count) };
    if values[0].tag != 4 { return 2; }
    let text = values[0].payload as *mut Vec<u8>;
    let len = if text.is_null() { 0 } else { unsafe { (&*text).len() } };
    unsafe { *out = BridgeValue { tag: 1, reserved: [0; 7], payload: len as u64 }; }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_raw_ptr(data: *const BridgeValue, count: u32, out: *mut BridgeValue) -> u32 {
    if count != 1 { return 1; }
    let values = unsafe { args(data, count) };
    if values[0].tag != 9 { return 2; }
    unsafe { *out = values[0]; }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_bad_count(_: *const BridgeValue, _: u32, _: *mut BridgeValue) -> u32 { 1 }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_bad_tag(_: *const BridgeValue, _: u32, _: *mut BridgeValue) -> u32 { 2 }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_interior_nul(_: *const BridgeValue, _: u32, _: *mut BridgeValue) -> u32 { 3 }
#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_bad_status(_: *const BridgeValue, _: u32, _: *mut BridgeValue) -> u32 { 77 }

#[unsafe(no_mangle)]
pub unsafe extern "C" fn test_malformed_result(_: *const BridgeValue, _: u32, out: *mut BridgeValue) -> u32 {
    unsafe { *out = BridgeValue { tag: 10, reserved: [0; 7], payload: 0 }; }
    0
}
"#;

    const STALE_SOURCE: &str = r#"
#[unsafe(no_mangle)]
pub extern "C" fn kira_foreign_adapter_abi_version_0() {}
"#;

    struct Fixture {
        directory: PathBuf,
        library: PathBuf,
    }

    impl Fixture {
        fn compile(source: &str) -> Self {
            let id = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
            let directory =
                std::env::temp_dir().join(format!("kira-dynamic-ffi-{}-{id}", std::process::id()));
            fs::create_dir_all(&directory).expect("fixture directory should be creatable");
            let source_path = directory.join("adapter.rs");
            fs::write(&source_path, source).expect("fixture source should be writable");
            let library = directory.join(format!(
                "{}fixture{}",
                std::env::consts::DLL_PREFIX,
                std::env::consts::DLL_SUFFIX
            ));
            let output = Command::new("rustc")
                .args(["--edition=2024", "--crate-type=cdylib"])
                .arg(&source_path)
                .arg("-o")
                .arg(&library)
                .output()
                .expect("rustc should run for adapter fixture");
            assert!(
                output.status.success(),
                "fixture failed to compile: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            Self { directory, library }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    #[test]
    fn loads_and_calls_a_generated_adapter() {
        let fixture = Fixture::compile(ADAPTER_SOURCE);
        let library = ForeignAdapterLibrary::load(&fixture.library).expect("fixture should load");
        let signature =
            ForeignSignature::new([ForeignType::I32, ForeignType::I32], ForeignType::I32);
        assert_eq!(
            library
                .call(
                    "test_add",
                    &signature,
                    &[ForeignArg::I32(20), ForeignArg::I32(22)]
                )
                .expect("adapter should run"),
            ForeignResult::I32(42)
        );
        assert_eq!(library.adapters.borrow().len(), 1);
        let _ = library
            .call(
                "test_add",
                &signature,
                &[ForeignArg::I32(1), ForeignArg::I32(2)],
            )
            .expect("cached adapter should run");
        assert_eq!(library.adapters.borrow().len(), 1);
    }

    #[test]
    fn cstring_and_raw_pointer_ownership_are_preserved() {
        let fixture = Fixture::compile(ADAPTER_SOURCE);
        let library = ForeignAdapterLibrary::load(&fixture.library).expect("fixture should load");
        let cstring = ForeignSignature::new([ForeignType::CString], ForeignType::U64);
        assert_eq!(
            library
                .call("test_cstring_len", &cstring, &[ForeignArg::CString("kira")])
                .expect("CString should be copied"),
            ForeignResult::U64(4)
        );
        let pointer = ForeignSignature::new([ForeignType::RawPtr], ForeignType::RawPtr);
        assert_eq!(
            library
                .call("test_raw_ptr", &pointer, &[ForeignArg::RawPtr(0x1234)])
                .expect("pointer should round trip"),
            ForeignResult::RawPtr(0x1234)
        );
    }

    #[test]
    fn rejects_a_stale_marker_and_malformed_symbol() {
        let stale = Fixture::compile(STALE_SOURCE);
        assert!(matches!(
            ForeignAdapterLibrary::load(&stale.library),
            Err(ForeignAdapterError::IncompatibleAbi { .. })
        ));

        let fixture = Fixture::compile(ADAPTER_SOURCE);
        let library = ForeignAdapterLibrary::load(&fixture.library).expect("fixture should load");
        let signature = ForeignSignature::new([], ForeignType::Void);
        assert!(matches!(
            library.call("bad\0symbol", &signature, &[]),
            Err(ForeignAdapterError::MalformedSymbol { .. })
        ));
    }

    #[test]
    fn reports_count_tag_nul_status_and_malformed_result_without_panicking() {
        let fixture = Fixture::compile(ADAPTER_SOURCE);
        let library = ForeignAdapterLibrary::load(&fixture.library).expect("fixture should load");
        let void = ForeignSignature::new([], ForeignType::Void);
        assert!(matches!(
            library.call("test_bad_count", &void, &[]),
            Err(ForeignAdapterError::Call(
                ForeignCallError::AdapterBadArgumentCount
            ))
        ));
        assert!(matches!(
            library.call("test_bad_tag", &void, &[]),
            Err(ForeignAdapterError::Call(
                ForeignCallError::AdapterBadArgumentTag
            ))
        ));
        assert!(matches!(
            library.call("test_interior_nul", &void, &[]),
            Err(ForeignAdapterError::Call(
                ForeignCallError::AdapterInteriorNul
            ))
        ));
        assert!(matches!(
            library.call("test_bad_status", &void, &[]),
            Err(ForeignAdapterError::Call(
                ForeignCallError::UnknownAdapterStatus(77)
            ))
        ));
        assert!(matches!(
            library.call("test_malformed_result", &void, &[]),
            Err(ForeignAdapterError::Call(
                ForeignCallError::MalformedResultTag { actual: 10, .. }
            ))
        ));

        let cstring = ForeignSignature::new([ForeignType::CString], ForeignType::Void);
        assert!(matches!(
            library.call("test_add", &cstring, &[ForeignArg::CString("a\0b")]),
            Err(ForeignAdapterError::Call(ForeignCallError::InteriorNul {
                index: 0
            }))
        ));
    }
}
