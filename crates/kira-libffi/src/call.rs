//! Argument storage and calls through a prepared libffi CIF.

use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{CStr, CString, c_void};
use std::ptr::NonNull;

use kira_runtime_abi::{
    ForeignAggregates, ForeignArg, ForeignCallError, ForeignResult, ForeignSignature, ForeignType,
    ForeignTypeSpec,
};

use crate::LibffiError;
use crate::raw::{RawFunction, RawLibffi};
use crate::types::PreparedCif;

/// A loaded copy of Kira's bundled libffi runtime.
#[derive(Clone)]
pub struct LibffiRuntime {
    pub(crate) api: std::sync::Arc<RawLibffi>,
}

/// One signature's prepared CIF, held by a host that calls it repeatedly.
pub struct PreparedCall {
    cif: PreparedCif,
}

// SAFETY: the type graph and the argument-type array are owned by this value
// and live on the heap, so moving it moves no address libffi holds. Nothing
// mutates the CIF after preparation: `ffi_call` reads it.
unsafe impl Send for PreparedCall {}
// SAFETY: as above — a shared reference grants only reads of the CIF, which is
// what a concurrent call through it performs.
unsafe impl Sync for PreparedCall {}

impl LibffiRuntime {
    /// Loads the libffi binary shipped with Kira.
    pub fn load() -> Result<Self, LibffiError> {
        Ok(Self {
            api: std::sync::Arc::new(RawLibffi::load()?),
        })
    }

    /// Loads libffi from a staged bundle path.
    pub fn load_from(path: impl AsRef<std::path::Path>) -> Result<Self, LibffiError> {
        Ok(Self {
            api: std::sync::Arc::new(RawLibffi::load_from(path.as_ref())?),
        })
    }

    /// The process's one loaded libffi, or `None` when it could not be loaded.
    ///
    /// Loading is a library open and nineteen symbol lookups, and a generated
    /// call site reaches this on every call: a frame of a graphics program is
    /// thousands of them. The load happens once and every caller after the
    /// first clones a handle to it.
    pub fn shared() -> Option<Self> {
        static SHARED: std::sync::OnceLock<Option<LibffiRuntime>> = std::sync::OnceLock::new();
        SHARED.get_or_init(|| Self::load().ok()).clone()
    }

    /// Calls `function` using the exact C signature and aggregate table.
    ///
    /// # Safety
    /// `function` must be an address in this process that is callable with the
    /// C ABI `signature` records, and every pointer `args` carries must stay
    /// valid for the duration of the call.
    pub unsafe fn call(
        &self,
        function: *mut c_void,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, LibffiError> {
        let prepared = self.prepare(signature, aggregates)?;
        // SAFETY: the CIF was just prepared from this signature, and the
        // caller's contract is this call's contract.
        unsafe { self.call_with(&prepared, function, signature, aggregates, args) }
    }

    /// Builds the CIF for one signature, to call through many times.
    ///
    /// A host that calls one import repeatedly prepares it once: the graph and
    /// `ffi_prep_cif` cost the same whether the signature is new or not, and a
    /// call site's signature never changes.
    pub fn prepare(
        &self,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
    ) -> Result<PreparedCall, LibffiError> {
        Ok(PreparedCall {
            cif: PreparedCif::new(&self.api, signature, aggregates)?,
        })
    }

    /// Calls `function` through a CIF [`prepare`](Self::prepare) built.
    ///
    /// # Safety
    /// `prepared` must be the CIF for `signature`, and the rest is
    /// [`call`](Self::call)'s contract.
    pub unsafe fn call_with(
        &self,
        prepared: &PreparedCall,
        function: *mut c_void,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, LibffiError> {
        if function.is_null() {
            return Err(LibffiError::NullFunction);
        }
        if args.len() != signature.parameters().len() {
            return Err(LibffiError::Call(ForeignCallError::ArgumentCount {
                expected: signature.parameters().len(),
                actual: args.len(),
            }));
        }
        let prepared = &prepared.cif;
        let mut strings = Vec::new();
        let mut storage = Vec::with_capacity(args.len());
        for (index, (expected, argument)) in signature
            .parameters()
            .iter()
            .copied()
            .zip(args.iter().copied())
            .enumerate()
        {
            if argument.spec() != expected {
                return Err(LibffiError::Call(ForeignCallError::ArgumentType {
                    index,
                    expected,
                    actual: argument.spec(),
                }));
            }
            let (size, align) = prepared.layout(expected)?;
            let mut value = AlignedBytes::new(size, align)?;
            write_argument(&mut value, argument, index, aggregates, &mut strings)?;
            storage.push(value);
        }
        let mut pointers: Vec<*mut c_void> = storage
            .iter_mut()
            .map(|value| value.as_mut_ptr().cast())
            .collect();
        let (result_size, result_align) = prepared.layout(signature.result())?;
        let mut result = if signature.result() == ForeignTypeSpec::Scalar(ForeignType::Void) {
            None
        } else {
            Some(AlignedBytes::new(
                result_size.max(RESULT_WORD),
                result_align.max(RESULT_WORD),
            )?)
        };
        // SAFETY: every pointer names storage with the size and alignment of its
        // corresponding `ffi_type`; the CIF and function pointer were checked
        // above and stay alive for the call.
        unsafe {
            let function: RawFunction = std::mem::transmute(function);
            (self.api.call)(
                std::ptr::from_ref(&prepared.cif).cast_mut(),
                function,
                result
                    .as_mut()
                    .map_or(std::ptr::null_mut(), AlignedBytes::as_mut_ptr)
                    .cast(),
                if pointers.is_empty() {
                    std::ptr::null_mut()
                } else {
                    pointers.as_mut_ptr()
                },
            );
        }
        lift_result(signature.result(), result.as_ref(), aggregates)
    }

    /// Calls a function whose arguments and result already occupy C-layout
    /// storage.
    ///
    /// This is the native backend entrypoint. It shares [`PreparedCif`] with
    /// the host path, but leaves storage ownership with the caller.
    ///
    /// # Safety
    /// `function` must use `signature`, `arguments` must point to one writable
    /// storage pointer per parameter, and `result` must point to writable result
    /// storage unless the result is void.
    pub unsafe fn call_raw(
        &self,
        function: *mut c_void,
        signature: &ForeignSignature,
        aggregates: &ForeignAggregates,
        arguments: *mut *mut c_void,
        result: *mut c_void,
    ) -> Result<(), LibffiError> {
        if function.is_null() {
            return Err(LibffiError::NullFunction);
        }
        if !matches!(
            signature.result(),
            ForeignTypeSpec::Scalar(ForeignType::Void)
        ) && result.is_null()
        {
            return Err(LibffiError::Call(ForeignCallError::InvalidCStringResult));
        }
        if !signature.parameters().is_empty() && arguments.is_null() {
            return Err(LibffiError::Call(ForeignCallError::ArgumentCount {
                expected: signature.parameters().len(),
                actual: 0,
            }));
        }
        let prepared = PreparedCif::new(&self.api, signature, aggregates)?;
        // SAFETY: the caller's contract, checked above, is this call's contract.
        unsafe { self.call_prepared(function, &prepared, signature.result(), arguments, result) }
    }

    /// Calls `function` through a CIF that was prepared earlier.
    ///
    /// Preparing one is a decode and an `ffi_prep_cif`, and a call site's
    /// signature never changes between calls: a generated site prepares once
    /// and calls through it for the life of the process.
    ///
    /// # Safety
    /// `prepared` must be the CIF for `function`'s signature, and the storage
    /// contract is [`call_raw`](Self::call_raw)'s.
    pub(crate) unsafe fn call_prepared(
        &self,
        function: *mut c_void,
        prepared: &PreparedCif,
        result_spec: ForeignTypeSpec,
        arguments: *mut *mut c_void,
        result: *mut c_void,
    ) -> Result<(), LibffiError> {
        let (result_size, result_align) = prepared.layout(result_spec)?;
        // Libffi writes a whole `ffi_arg` word for a result narrower than one,
        // so a caller's exactly-sized storage is not the storage it may write
        // into. The call lands in a word-wide buffer and the declared bytes are
        // copied back out of its low end.
        let mut widened = if matches!(result_spec, ForeignTypeSpec::Scalar(ForeignType::Void))
            || result_size >= RESULT_WORD
        {
            None
        } else {
            Some(AlignedBytes::new(
                RESULT_WORD,
                result_align.max(RESULT_WORD),
            )?)
        };
        let target = widened
            .as_mut()
            .map_or(result, |bytes| bytes.as_mut_ptr().cast());
        // SAFETY: the caller supplies exactly the storage described by the CIF;
        // the function pointer is checked above and the prepared graph lives
        // through the call. `ffi_call` reads the CIF and writes only the result
        // and argument storage, so a shared borrow is what it needs — and it is
        // what re-entrant calls through one prepared site require.
        unsafe {
            let function: RawFunction = std::mem::transmute(function);
            let cif = std::ptr::from_ref(&prepared.cif).cast_mut();
            (self.api.call)(cif, function, target, arguments);
        }
        if let Some(widened) = widened {
            // SAFETY: `result` is the caller's writable storage for exactly
            // `result_size` bytes, and the buffer read from is wider than that.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    widened.as_slice().as_ptr(),
                    result.cast::<u8>(),
                    result_size,
                );
            }
        }
        Ok(())
    }
}

/// The word libffi widens a narrow scalar result into.
const RESULT_WORD: usize = size_of::<usize>();

fn write_argument(
    destination: &mut AlignedBytes,
    argument: ForeignArg<'_>,
    index: usize,
    aggregates: &ForeignAggregates,
    strings: &mut Vec<CString>,
) -> Result<(), LibffiError> {
    match argument {
        ForeignArg::Void => Ok(()),
        ForeignArg::I8(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::I16(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::I32(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::I64(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::U8(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::U16(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::U32(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::U64(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::Bool(value) => destination.write(&[u8::from(value)]),
        ForeignArg::F32(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::F64(value) => destination.write(&value.to_ne_bytes()),
        ForeignArg::RawPtr(value) => {
            if (value as usize) as u64 != value {
                return Err(LibffiError::Call(ForeignCallError::RawPointerOutOfRange {
                    value,
                }));
            }
            destination.write(&(value as usize).to_ne_bytes())
        }
        ForeignArg::CString(value) => {
            let string = CString::new(value)
                .map_err(|_| LibffiError::Call(ForeignCallError::InteriorNul { index }))?;
            let pointer = string.as_ptr() as usize;
            strings.push(string);
            destination.write(&pointer.to_ne_bytes())
        }
        ForeignArg::Aggregate { id, bytes } => {
            let expected = aggregates
                .layout_of(id, kira_runtime_abi::ForeignPointerWidth::HOST)?
                .size as usize;
            if bytes.len() != expected || destination.len() != expected {
                return Err(LibffiError::Call(ForeignCallError::AggregateSize {
                    index,
                    expected,
                    actual: bytes.len(),
                }));
            }
            destination.write(bytes)
        }
    }
}

fn lift_result(
    result_spec: ForeignTypeSpec,
    result: Option<&AlignedBytes>,
    aggregates: &ForeignAggregates,
) -> Result<ForeignResult, LibffiError> {
    let Some(result) = result else {
        return Ok(ForeignResult::Void);
    };
    let bytes = result.as_slice();
    let value = match result_spec {
        ForeignTypeSpec::Scalar(ForeignType::I8) => ForeignResult::I8(bytes[0] as i8),
        ForeignTypeSpec::Scalar(ForeignType::I16) => ForeignResult::I16(i16::from_ne_bytes(
            bytes[..2].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::I32) => ForeignResult::I32(i32::from_ne_bytes(
            bytes[..4].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::I64) => ForeignResult::I64(i64::from_ne_bytes(
            bytes[..8].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::U8) => ForeignResult::U8(bytes[0]),
        ForeignTypeSpec::Scalar(ForeignType::U16) => ForeignResult::U16(u16::from_ne_bytes(
            bytes[..2].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::U32) => ForeignResult::U32(u32::from_ne_bytes(
            bytes[..4].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::U64) => ForeignResult::U64(u64::from_ne_bytes(
            bytes[..8].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::Bool) => ForeignResult::Bool(bytes[0] != 0),
        ForeignTypeSpec::Scalar(ForeignType::F32) => ForeignResult::F32(f32::from_ne_bytes(
            bytes[..4].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::F64) => ForeignResult::F64(f64::from_ne_bytes(
            bytes[..8].try_into().map_err(|_| malformed())?,
        )),
        ForeignTypeSpec::Scalar(ForeignType::RawPtr) => {
            let value = read_usize(bytes)? as u64;
            ForeignResult::RawPtr(value)
        }
        ForeignTypeSpec::Scalar(ForeignType::CString) => {
            let pointer = read_usize(bytes)? as *const std::ffi::c_char;
            if pointer.is_null() {
                ForeignResult::CString(String::new())
            } else {
                // SAFETY: the C function returned a pointer valid until the
                // call returned; this copy is performed before the call frame
                // is released.
                let text = unsafe { CStr::from_ptr(pointer) }
                    .to_str()
                    .map_err(|_| LibffiError::Call(ForeignCallError::InvalidCStringResult))?;
                ForeignResult::CString(text.to_owned())
            }
        }
        ForeignTypeSpec::Scalar(ForeignType::Void) => ForeignResult::Void,
        ForeignTypeSpec::Aggregate(id) => {
            let expected = aggregates.layout_of(id, kira_runtime_abi::ForeignPointerWidth::HOST)?;
            if bytes.len() != expected.size as usize {
                return Err(LibffiError::Call(ForeignCallError::AggregateSize {
                    index: 0,
                    expected: expected.size as usize,
                    actual: bytes.len(),
                }));
            }
            ForeignResult::Aggregate {
                id,
                bytes: bytes.to_vec().into_boxed_slice(),
            }
        }
    };
    Ok(value)
}

fn read_usize(bytes: &[u8]) -> Result<usize, LibffiError> {
    match size_of::<usize>() {
        4 => Ok(u32::from_ne_bytes(bytes[..4].try_into().map_err(|_| malformed())?) as usize),
        8 => Ok(u64::from_ne_bytes(bytes[..8].try_into().map_err(|_| malformed())?) as usize),
        _ => Err(malformed()),
    }
}

fn malformed() -> LibffiError {
    LibffiError::Call(ForeignCallError::InvalidCStringResult)
}

struct AlignedBytes {
    pointer: NonNull<u8>,
    layout: Layout,
    length: usize,
}

impl AlignedBytes {
    fn new(size: usize, alignment: usize) -> Result<Self, LibffiError> {
        let length = size.max(1);
        let layout = Layout::from_size_align(length, alignment.max(1))
            .map_err(|_| LibffiError::Storage { size, alignment })?;
        // SAFETY: `layout` is valid and the allocation is released by Drop with
        // the identical layout.
        let pointer = NonNull::new(unsafe { alloc(layout) })
            .ok_or(LibffiError::Storage { size, alignment })?;
        // SAFETY: the allocation above is `length` writable bytes.
        unsafe { pointer.as_ptr().write_bytes(0, length) };
        Ok(Self {
            pointer,
            layout,
            length,
        })
    }

    fn len(&self) -> usize {
        self.length
    }

    fn as_mut_ptr(&mut self) -> *mut u8 {
        self.pointer.as_ptr()
    }

    fn as_slice(&self) -> &[u8] {
        // SAFETY: the allocation is live for the returned borrow and has exactly
        // `length` initialized bytes.
        unsafe { std::slice::from_raw_parts(self.pointer.as_ptr(), self.length) }
    }

    fn write(&mut self, bytes: &[u8]) -> Result<(), LibffiError> {
        if bytes.len() > self.length {
            return Err(LibffiError::Storage {
                size: self.length,
                alignment: self.layout.align(),
            });
        }
        // SAFETY: both slices are valid for `bytes.len()` and do not overlap.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.pointer.as_ptr(), bytes.len())
        };
        Ok(())
    }
}

impl Drop for AlignedBytes {
    fn drop(&mut self) {
        // SAFETY: this is the allocation returned by `alloc` with this layout.
        unsafe { dealloc(self.pointer.as_ptr(), self.layout) };
    }
}
