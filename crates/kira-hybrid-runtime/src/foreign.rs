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
    BridgeData, BridgeValue, ForeignAdapterFn, ForeignAdapterStatus, ForeignArg, ForeignCallError,
    ForeignResult, ForeignSignature, ForeignType,
};

use crate::library::NativeLibrary;

/// Calls one adapter with `args`, marshalling through `library`'s allocator.
///
/// Every transient `CString` is copied into a fresh handle from `library` and
/// freed on every exit path — a clean return, a tag mismatch, or a status error.
/// A `CString` result is refused: adapter ABI version 1 does not own returned C
/// strings.
///
/// # Safety
/// `adapter` must be one of `library`'s bound adapters, and `signature` must be
/// the one it was generated for — the adapter reads its arguments by position
/// and cannot check.
pub unsafe fn call_adapter(
    library: &NativeLibrary,
    adapter: ForeignAdapterFn,
    signature: &ForeignSignature,
    args: &[ForeignArg<'_>],
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

    let mut strings = TransientStrings::new(library);
    let mut lowered = Vec::with_capacity(args.len());
    for (index, (&expected, &argument)) in
        signature.parameters().iter().zip(args.iter()).enumerate()
    {
        lowered.push(lower_argument(expected, argument, index, &mut strings)?);
    }

    let mut out = BridgeValue::VOID;
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
        ForeignAdapterStatus::SUCCESS => lift_result(signature.result(), out),
        ForeignAdapterStatus::BAD_ARGUMENT_COUNT => Err(ForeignCallError::AdapterBadArgumentCount),
        ForeignAdapterStatus::BAD_ARGUMENT_TAG => Err(ForeignCallError::AdapterBadArgumentTag),
        ForeignAdapterStatus::INTERIOR_NUL => Err(ForeignCallError::AdapterInteriorNul),
        ForeignAdapterStatus::MALFORMED_RESULT => Err(ForeignCallError::AdapterMalformedResult),
        ForeignAdapterStatus(other) => Err(ForeignCallError::UnknownAdapterStatus(other)),
    }
}

/// Lowers one borrowed argument into the bridge value the adapter reads.
fn lower_argument(
    expected: ForeignType,
    argument: ForeignArg<'_>,
    index: usize,
    strings: &mut TransientStrings<'_>,
) -> Result<BridgeValue, ForeignCallError> {
    let actual = argument.foreign_type();
    if actual != expected {
        return Err(ForeignCallError::ArgumentType {
            index,
            expected,
            actual,
        });
    }
    let data = match argument {
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
        ForeignType::CString => {
            return Err(ForeignCallError::UnsupportedResultType(
                ForeignType::CString,
            ));
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
