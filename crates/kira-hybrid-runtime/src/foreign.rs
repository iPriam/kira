//! Calling a generated foreign adapter bound inside the hybrid native half.
//!
//! The runtime half reaches C exactly as the VM sidecar host does — through one
//! uniform adapter per import that speaks [`BridgeValue`] — but binds it out of
//! the *same* dylib the `@Native` trampolines live in, and copies every
//! transient `CString` through that library's own allocator. That is the
//! single-copy rule made real: a runtime-half foreign call and a native-half one
//! reach one instance of the C code, never two.
//!
//! The marshalling mirrors `kira-dynamic-ffi`'s adapter call, spelled again here
//! rather than shared because that crate `dlopen`s its own sidecar — which is
//! precisely the second copy this path exists to avoid.

use kira_runtime_abi::{
    BridgeData, BridgeValue, BridgeValueTag, ForeignAdapterFn, ForeignAdapterStatus,
    ForeignAggregates, ForeignArg, ForeignCallError, ForeignPointerWidth, ForeignResult,
    ForeignSignature, ForeignType, ForeignTypeSpec,
};

use crate::library::NativeLibrary;

/// The pointer width of this process, which is the target the loaded native
/// half was compiled for.
const HOST_POINTER_WIDTH: ForeignPointerWidth = ForeignPointerWidth::HOST;

/// Calls one adapter with `args`, marshalling through `library`'s allocator.
///
/// Every transient `CString` is copied into a fresh handle from `library` and
/// freed on every exit path — a clean return, a tag mismatch, or a status error.
/// A `CString` **result** is copied in the other direction, out of storage the
/// callee keeps, so nothing here holds C memory or frees any.
///
/// # Safety
/// `adapter` must be one of `library`'s bound adapters, and `signature` must be
/// the one it was generated for — the adapter reads its arguments by position
/// and cannot check.
pub unsafe fn call_adapter(
    library: &NativeLibrary,
    adapter: ForeignAdapterFn,
    signature: &ForeignSignature,
    aggregates: &ForeignAggregates,
    args: &[ForeignArg<'_>],
) -> Result<ForeignResult, ForeignCallError> {
    if args.len() != signature.parameters().len() {
        return Err(ForeignCallError::ArgumentCount {
            expected: signature.parameters().len(),
            actual: args.len(),
        });
    }
    let count = u32::try_from(args.len())
        .map_err(|_| ForeignCallError::TooManyArguments { actual: args.len() })?;

    let mut strings = TransientStrings::new(library);
    let mut lowered = Vec::with_capacity(args.len());
    for (index, (&expected, &argument)) in
        signature.parameters().iter().zip(args.iter()).enumerate()
    {
        lowered.push(lower_argument(
            expected,
            argument,
            index,
            aggregates,
            &mut strings,
        )?);
    }

    // An aggregate result is written into storage this caller owns, so the
    // native half never allocates one and a failed call leaves it untouched.
    let mut result_buffer = match signature.result().aggregate() {
        Some(id) => vec![0u8; aggregates.layout_of(id, HOST_POINTER_WIDTH)?.size as usize],
        None => Vec::new(),
    };
    let mut out = match signature.result().aggregate() {
        Some(_) => BridgeValue::new(BridgeValueTag::AGGREGATE, result_buffer.as_mut_ptr() as u64),
        None => BridgeValue::VOID,
    };
    let pointer = if lowered.is_empty() {
        std::ptr::null()
    } else {
        lowered.as_ptr()
    };
    // SAFETY: `pointer` covers exactly `count` initialized bridge values (or is
    // null for zero), `out` is writable for this call, and the caller vouches
    // `adapter` is one of this library's, whose ABI marker was proven at load.
    let status = unsafe { adapter(pointer, count, &mut out) };
    drop(strings);

    match status {
        ForeignAdapterStatus::SUCCESS => {
            lift_result(signature.result(), out, result_buffer, library)
        }
        ForeignAdapterStatus::BAD_ARGUMENT_COUNT => Err(ForeignCallError::AdapterBadArgumentCount),
        ForeignAdapterStatus::BAD_ARGUMENT_TAG => Err(ForeignCallError::AdapterBadArgumentTag),
        ForeignAdapterStatus::INTERIOR_NUL => Err(ForeignCallError::AdapterInteriorNul),
        ForeignAdapterStatus::MALFORMED_RESULT => Err(ForeignCallError::AdapterMalformedResult),
        ForeignAdapterStatus::BAD_RESULT_SLOT => Err(ForeignCallError::AdapterBadResultSlot),
        ForeignAdapterStatus(other) => Err(ForeignCallError::UnknownAdapterStatus(other)),
    }
}

/// Lowers one borrowed argument into the bridge value the adapter reads.
fn lower_argument(
    expected: ForeignTypeSpec,
    argument: ForeignArg<'_>,
    index: usize,
    aggregates: &ForeignAggregates,
    strings: &mut TransientStrings<'_>,
) -> Result<BridgeValue, ForeignCallError> {
    let actual = argument.spec();
    if actual != expected {
        return Err(ForeignCallError::ArgumentType {
            index,
            expected,
            actual,
        });
    }
    let data = match argument {
        // An aggregate crosses as a borrowed pointer to its C-layout bytes,
        // which no `BridgeData` variant models: those all fit one payload word.
        ForeignArg::Aggregate { id, bytes } => {
            let expected_size = aggregates.layout_of(id, HOST_POINTER_WIDTH)?.size as usize;
            if bytes.len() != expected_size {
                return Err(ForeignCallError::AggregateSize {
                    index,
                    expected: expected_size,
                    actual: bytes.len(),
                });
            }
            return Ok(BridgeValue::new(
                BridgeValueTag::AGGREGATE,
                bytes.as_ptr() as u64,
            ));
        }
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
    Ok(BridgeValue::encode(data))
}

/// Lifts what the adapter wrote into an owned foreign result.
///
/// `result_buffer` is the caller's aggregate storage, empty for every scalar
/// result; a successful aggregate call hands its bytes straight out.
fn lift_result(
    expected: ForeignTypeSpec,
    value: BridgeValue,
    result_buffer: Vec<u8>,
    library: &NativeLibrary,
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
    let expected = match expected {
        ForeignTypeSpec::Scalar(ty) => ty,
        // The adapter fills the caller's buffer in place, so the payload it
        // leaves must still be the pointer handed in.
        ForeignTypeSpec::Aggregate(id) => {
            if value.payload != result_buffer.as_ptr() as u64 {
                return Err(ForeignCallError::AdapterMalformedResult);
            }
            return Ok(ForeignResult::Aggregate {
                id,
                bytes: result_buffer.into_boxed_slice(),
            });
        }
    };
    Ok(match expected {
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
        // The adapter already copied the callee's bytes into a string handle
        // from the loaded native half's own runtime — the same handle shape a
        // transient argument crosses as, in the other direction. Read and free
        // it through that library, because it is that library's allocation.
        ForeignType::CString => {
            check_pointer_width(value.payload)?;
            // SAFETY: the adapter returned this as a live handle from the same
            // library; it is read here and freed exactly once.
            let text = unsafe { library.take_string(value.payload) }
                .map_err(|_| ForeignCallError::AdapterMalformedResult)?;
            ForeignResult::CString(text)
        }
    })
}

/// Rejects a pointer word that does not fit this target's pointer width.
fn check_pointer_width(value: u64) -> Result<(), ForeignCallError> {
    if (value as usize) as u64 != value {
        return Err(ForeignCallError::RawPointerOutOfRange { value });
    }
    Ok(())
}

/// The transient `CString` handles a call allocates, freed on drop.
struct TransientStrings<'a> {
    library: &'a NativeLibrary,
    handles: Vec<u64>,
}

impl<'a> TransientStrings<'a> {
    fn new(library: &'a NativeLibrary) -> Self {
        Self {
            library,
            handles: Vec::new(),
        }
    }

    fn copy(&mut self, text: &str) -> u64 {
        let handle = self.library.new_string(text);
        self.handles.push(handle);
        handle
    }
}

impl Drop for TransientStrings<'_> {
    fn drop(&mut self) {
        for handle in self.handles.drain(..) {
            // SAFETY: every handle came from this library's `new_string`, is
            // still live, and is freed exactly once here on every exit path.
            unsafe { self.library.free_string(handle) };
        }
    }
}
