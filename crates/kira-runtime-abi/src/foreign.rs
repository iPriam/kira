//! The safe Rust vocabulary and C ABI for calls through generated foreign adapters.

use crate::aggregate::{ForeignAggregateError, ForeignAggregateId};
use crate::{BridgeValue, BridgeValueTag};

/// The ABI a foreign declaration uses.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignAbi {
    /// The platform C ABI.
    C = 0,
}

impl ForeignAbi {
    /// Returns the append-only serialized byte for this ABI.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Decodes an ABI from its serialized byte.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::C),
            _ => None,
        }
    }
}

/// A type permitted in a foreign declaration.
///
/// The serialized tags are append-only. They describe the exact C-width seam;
/// bare Kira `Int`, `Float`, and `String` are deliberately absent.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignType {
    /// No value.
    Void = 0,
    /// An 8-bit signed integer.
    I8 = 1,
    /// A 16-bit signed integer.
    I16 = 2,
    /// A 32-bit signed integer.
    I32 = 3,
    /// A 64-bit signed integer.
    I64 = 4,
    /// An 8-bit unsigned integer.
    U8 = 5,
    /// A 16-bit unsigned integer.
    U16 = 6,
    /// A 32-bit unsigned integer.
    U32 = 7,
    /// A 64-bit unsigned integer.
    U64 = 8,
    /// A C `_Bool` value.
    Bool = 9,
    /// A 32-bit IEEE-754 float.
    F32 = 10,
    /// A 64-bit IEEE-754 float.
    F64 = 11,
    /// An opaque target-width pointer word.
    RawPtr = 12,
    /// A borrowed NUL-terminated C string parameter.
    CString = 13,
}

impl ForeignType {
    /// Returns the append-only serialized byte for this type.
    pub const fn tag(self) -> u8 {
        self as u8
    }

    /// Decodes a foreign type from its serialized byte.
    pub const fn from_tag(tag: u8) -> Option<Self> {
        match tag {
            0 => Some(Self::Void),
            1 => Some(Self::I8),
            2 => Some(Self::I16),
            3 => Some(Self::I32),
            4 => Some(Self::I64),
            5 => Some(Self::U8),
            6 => Some(Self::U16),
            7 => Some(Self::U32),
            8 => Some(Self::U64),
            9 => Some(Self::Bool),
            10 => Some(Self::F32),
            11 => Some(Self::F64),
            12 => Some(Self::RawPtr),
            13 => Some(Self::CString),
            _ => None,
        }
    }

    /// Returns the bridge tag used by a generated adapter for this type.
    pub const fn bridge_tag(self) -> BridgeValueTag {
        match self {
            Self::Void => BridgeValueTag::VOID,
            Self::I8
            | Self::I16
            | Self::I32
            | Self::I64
            | Self::U8
            | Self::U16
            | Self::U32
            | Self::U64 => BridgeValueTag::INT,
            Self::Bool => BridgeValueTag::BOOL,
            Self::F32 | Self::F64 => BridgeValueTag::FLOAT,
            Self::RawPtr => BridgeValueTag::RAW_PTR,
            Self::CString => BridgeValueTag::STRING,
        }
    }
}

/// One position in a foreign signature: a seam scalar, or a C-layout aggregate
/// named by its index in the program's aggregate table.
///
/// [`ForeignType`] stays scalar-only so its pinned tags never move. A spec
/// serializes as one tag byte — a scalar's own tag, or
/// [`ForeignTypeSpec::AGGREGATE_TAG`] followed by the table index — so a decoder
/// written before aggregates existed rejects the new byte by name instead of
/// misreading the index that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForeignTypeSpec {
    /// A fixed-width seam scalar.
    Scalar(ForeignType),
    /// A C-layout aggregate, by index into the program's aggregate table.
    Aggregate(ForeignAggregateId),
}

impl ForeignTypeSpec {
    /// The appended tag byte that introduces an aggregate position.
    pub const AGGREGATE_TAG: u8 = 14;

    /// Returns the serialized tag byte for this position.
    pub const fn tag(self) -> u8 {
        match self {
            Self::Scalar(ty) => ty.tag(),
            Self::Aggregate(_) => Self::AGGREGATE_TAG,
        }
    }

    /// Returns the seam scalar, or `None` when this position is an aggregate.
    pub const fn scalar(self) -> Option<ForeignType> {
        match self {
            Self::Scalar(ty) => Some(ty),
            Self::Aggregate(_) => None,
        }
    }

    /// Returns the aggregate index, or `None` when this position is a scalar.
    pub const fn aggregate(self) -> Option<ForeignAggregateId> {
        match self {
            Self::Scalar(_) => None,
            Self::Aggregate(id) => Some(id),
        }
    }

    /// Returns the bridge tag a generated adapter uses for this position.
    pub const fn bridge_tag(self) -> BridgeValueTag {
        match self {
            Self::Scalar(ty) => ty.bridge_tag(),
            Self::Aggregate(_) => BridgeValueTag::AGGREGATE,
        }
    }
}

impl From<ForeignType> for ForeignTypeSpec {
    fn from(ty: ForeignType) -> Self {
        Self::Scalar(ty)
    }
}

/// A scalar compares equal to the position that names it, so a caller holding
/// the seam vocabulary need not wrap one to ask.
impl PartialEq<ForeignType> for ForeignTypeSpec {
    fn eq(&self, other: &ForeignType) -> bool {
        self.scalar() == Some(*other)
    }
}

/// The complete type signature of one foreign import.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignSignature {
    parameters: Box<[ForeignTypeSpec]>,
    result: ForeignTypeSpec,
}

impl ForeignSignature {
    /// Creates a signature from its parameter and result positions.
    pub fn new(
        parameters: impl Into<Box<[ForeignTypeSpec]>>,
        result: impl Into<ForeignTypeSpec>,
    ) -> Self {
        Self {
            parameters: parameters.into(),
            result: result.into(),
        }
    }

    /// Creates a signature whose every position is a seam scalar.
    ///
    /// The common shape, and the only one a program with no C-layout aggregate
    /// in any signature ever has.
    pub fn scalars(parameters: impl IntoIterator<Item = ForeignType>, result: ForeignType) -> Self {
        Self::new(
            parameters
                .into_iter()
                .map(ForeignTypeSpec::Scalar)
                .collect::<Vec<_>>(),
            result,
        )
    }

    /// Returns the parameter positions in declaration order.
    pub fn parameters(&self) -> &[ForeignTypeSpec] {
        &self.parameters
    }

    /// Returns the result position.
    pub const fn result(&self) -> ForeignTypeSpec {
        self.result
    }

    /// Returns whether any position in this signature is an aggregate.
    ///
    /// This is what decides whether the backend generates a C shim for the
    /// import: a scalar-only signature reaches its C symbol directly.
    pub fn has_aggregate(&self) -> bool {
        self.result.aggregate().is_some()
            || self
                .parameters
                .iter()
                .any(|spec| spec.aggregate().is_some())
    }
}

/// One declared foreign function before backend-specific adapter naming.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignImport {
    library: String,
    symbol: String,
    abi: ForeignAbi,
    signature: ForeignSignature,
}

impl ForeignImport {
    /// Creates a foreign import.
    pub fn new(
        library: impl Into<String>,
        symbol: impl Into<String>,
        abi: ForeignAbi,
        signature: ForeignSignature,
    ) -> Self {
        Self {
            library: library.into(),
            symbol: symbol.into(),
            abi,
            signature,
        }
    }

    /// Returns the declared native-library name.
    pub fn library(&self) -> &str {
        &self.library
    }

    /// Returns the C symbol named by the declaration.
    pub fn symbol(&self) -> &str {
        &self.symbol
    }

    /// Returns the declaration's ABI.
    pub const fn abi(&self) -> ForeignAbi {
        self.abi
    }

    /// Returns the declaration's exact-width signature.
    pub const fn signature(&self) -> &ForeignSignature {
        &self.signature
    }
}

/// A borrowed argument supplied to a foreign call.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ForeignArg<'a> {
    /// The unit value.
    Void,
    /// An 8-bit signed integer.
    I8(i8),
    /// A 16-bit signed integer.
    I16(i16),
    /// A 32-bit signed integer.
    I32(i32),
    /// A 64-bit signed integer.
    I64(i64),
    /// An 8-bit unsigned integer.
    U8(u8),
    /// A 16-bit unsigned integer.
    U16(u16),
    /// A 32-bit unsigned integer.
    U32(u32),
    /// A 64-bit unsigned integer.
    U64(u64),
    /// A C `_Bool` value.
    Bool(bool),
    /// A 32-bit IEEE-754 float.
    F32(f32),
    /// A 64-bit IEEE-754 float.
    F64(f64),
    /// An opaque target-width pointer word, zero-extended in the bridge payload.
    RawPtr(u64),
    /// UTF-8 bytes borrowed for this call and copied to transient C storage.
    CString(&'a str),
    /// A C-layout aggregate, borrowed for this call.
    Aggregate {
        /// The aggregate's index in the program's table.
        id: ForeignAggregateId,
        /// Exactly the aggregate's `sizeof` bytes, in the target's C layout.
        bytes: &'a [u8],
    },
}

impl ForeignArg<'_> {
    /// Returns this argument's exact seam position.
    pub const fn spec(self) -> ForeignTypeSpec {
        ForeignTypeSpec::Scalar(match self {
            Self::Void => ForeignType::Void,
            Self::I8(_) => ForeignType::I8,
            Self::I16(_) => ForeignType::I16,
            Self::I32(_) => ForeignType::I32,
            Self::I64(_) => ForeignType::I64,
            Self::U8(_) => ForeignType::U8,
            Self::U16(_) => ForeignType::U16,
            Self::U32(_) => ForeignType::U32,
            Self::U64(_) => ForeignType::U64,
            Self::Bool(_) => ForeignType::Bool,
            Self::F32(_) => ForeignType::F32,
            Self::F64(_) => ForeignType::F64,
            Self::RawPtr(_) => ForeignType::RawPtr,
            Self::CString(_) => ForeignType::CString,
            Self::Aggregate { id, .. } => return ForeignTypeSpec::Aggregate(id),
        })
    }
}

/// An owned value returned from a foreign call.
///
/// `CString` is intentionally absent: returned C-string ownership is not part
/// of the adapter ABI.
#[derive(Debug, Clone, PartialEq)]
pub enum ForeignResult {
    /// The unit value.
    Void,
    /// An 8-bit signed integer.
    I8(i8),
    /// A 16-bit signed integer.
    I16(i16),
    /// A 32-bit signed integer.
    I32(i32),
    /// A 64-bit signed integer.
    I64(i64),
    /// An 8-bit unsigned integer.
    U8(u8),
    /// A 16-bit unsigned integer.
    U16(u16),
    /// A 32-bit unsigned integer.
    U32(u32),
    /// A 64-bit unsigned integer.
    U64(u64),
    /// A C `_Bool` value.
    Bool(bool),
    /// A 32-bit IEEE-754 float, rounded at the C boundary.
    F32(f32),
    /// A 64-bit IEEE-754 float.
    F64(f64),
    /// An opaque target-width pointer word, zero-extended in the bridge payload.
    RawPtr(u64),
    /// A C-layout aggregate, owned: exactly its `sizeof` bytes in target layout.
    Aggregate {
        /// The aggregate's index in the program's table.
        id: ForeignAggregateId,
        /// The returned bytes, copied out of the call's result buffer.
        bytes: Box<[u8]>,
    },
}

impl ForeignResult {
    /// Returns this result's exact seam position.
    pub const fn spec(&self) -> ForeignTypeSpec {
        ForeignTypeSpec::Scalar(match self {
            Self::Void => ForeignType::Void,
            Self::I8(_) => ForeignType::I8,
            Self::I16(_) => ForeignType::I16,
            Self::I32(_) => ForeignType::I32,
            Self::I64(_) => ForeignType::I64,
            Self::U8(_) => ForeignType::U8,
            Self::U16(_) => ForeignType::U16,
            Self::U32(_) => ForeignType::U32,
            Self::U64(_) => ForeignType::U64,
            Self::Bool(_) => ForeignType::Bool,
            Self::F32(_) => ForeignType::F32,
            Self::F64(_) => ForeignType::F64,
            Self::RawPtr(_) => ForeignType::RawPtr,
            Self::Aggregate { id, .. } => return ForeignTypeSpec::Aggregate(*id),
        })
    }
}

/// A status returned by a version-1 generated foreign adapter.
///
/// This is an open C value, not a Rust enum: a malformed or newer library may
/// return an unknown integer, which the loader must reject without undefined
/// behaviour.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ForeignAdapterStatus(pub u32);

impl ForeignAdapterStatus {
    /// The call completed and wrote a result.
    pub const SUCCESS: Self = Self(0);
    /// The adapter received a different argument count than its signature.
    pub const BAD_ARGUMENT_COUNT: Self = Self(1);
    /// At least one argument carried the wrong bridge tag.
    pub const BAD_ARGUMENT_TAG: Self = Self(2);
    /// A `CString` argument contained an interior NUL byte.
    pub const INTERIOR_NUL: Self = Self(3);
    /// The adapter could not encode a valid result.
    pub const MALFORMED_RESULT: Self = Self(4);
    /// The caller did not present a writable aggregate buffer in the out slot.
    pub const BAD_RESULT_SLOT: Self = Self(5);
}

/// The version of the generated foreign-adapter ABI.
///
/// Version 2 added aggregates: an aggregate argument arrives as a pointer to
/// C-layout bytes under [`BridgeValueTag::AGGREGATE`], and an aggregate result
/// is written into a buffer the *caller* presents in the out slot before the
/// call. Version 1 adapters have neither, and a version-1 caller would hand a
/// version-2 adapter an out slot it does not fill — so the marker name carries
/// the version and a mismatch fails the link rather than the run.
pub const FOREIGN_ADAPTER_ABI_VERSION: u32 = 2;

/// The versioned marker symbol every generated adapter library must export.
pub const FOREIGN_ADAPTER_ABI_MARKER: &str = "kira_foreign_adapter_abi_version_2";

/// The string-allocation helper resolved from an adapter library.
pub const FOREIGN_STRING_NEW_SYMBOL: &str = "kira_rt_str_new";
/// The string-free helper resolved from an adapter library.
pub const FOREIGN_STRING_FREE_SYMBOL: &str = "kira_rt_str_free";
/// The string-data helper resolved from an adapter library.
pub const FOREIGN_STRING_DATA_SYMBOL: &str = "kira_rt_str_data";
/// The string-length helper resolved from an adapter library.
pub const FOREIGN_STRING_LEN_SYMBOL: &str = "kira_rt_str_len";

/// A generated adapter's uniform C-call entrypoint.
///
/// An aggregate argument's payload is a pointer to the aggregate's C-layout
/// bytes, readable for the duration of the call. When the import's result is an
/// aggregate, `out` must arrive already carrying [`BridgeValueTag::AGGREGATE`]
/// and a pointer to a writable buffer of at least the aggregate's `sizeof`
/// bytes; the adapter fills that buffer and leaves the same tag and pointer in
/// place. Any other result type ignores `out`'s incoming contents.
///
/// # Safety
/// `args` must address `count` readable [`BridgeValue`]s, or be null when the
/// count is zero. `out` must address one writable [`BridgeValue`], and — for an
/// aggregate result — must already point at a buffer of the required size.
pub type ForeignAdapterFn = unsafe extern "C" fn(
    args: *const BridgeValue,
    count: u32,
    out: *mut BridgeValue,
) -> ForeignAdapterStatus;

/// Why a safe foreign call could not complete.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ForeignCallError {
    /// This host has no generated foreign-adapter library.
    #[error("this host has no foreign-call adapter loaded")]
    NoForeignHost,
    /// The caller supplied the wrong number of arguments.
    #[error("foreign call expected {expected} arguments but received {actual}")]
    ArgumentCount {
        /// The signature's count.
        expected: usize,
        /// The supplied count.
        actual: usize,
    },
    /// One argument does not match the declaration's exact-width type.
    #[error("foreign argument {index} has type {actual:?}, expected {expected:?}")]
    ArgumentType {
        /// The argument position.
        index: usize,
        /// The signature's position.
        expected: ForeignTypeSpec,
        /// The supplied value's position.
        actual: ForeignTypeSpec,
    },
    /// An aggregate argument carries a different byte count than its layout.
    #[error("foreign aggregate argument {index} carries {actual} bytes, expected {expected}")]
    AggregateSize {
        /// The argument position.
        index: usize,
        /// The aggregate's `sizeof` on this target.
        expected: usize,
        /// The supplied byte count.
        actual: usize,
    },
    /// The program's aggregate table cannot be laid out for this target.
    #[error("foreign aggregate layout: {0}")]
    AggregateLayout(#[from] ForeignAggregateError),
    /// The adapter reported that the caller's aggregate result slot was unusable.
    #[error("foreign adapter rejected the aggregate result buffer it was given")]
    AdapterBadResultSlot,
    /// The argument count does not fit the adapter ABI's `u32` count.
    #[error("foreign call has {actual} arguments, exceeding the adapter ABI limit")]
    TooManyArguments {
        /// The supplied count.
        actual: usize,
    },
    /// A borrowed C-string argument contains an interior NUL byte.
    #[error("foreign CString argument {index} contains an interior NUL byte")]
    InteriorNul {
        /// The argument position.
        index: usize,
    },
    /// A raw pointer word cannot fit the current target's pointer width.
    #[error("raw pointer value {value:#x} does not fit this target's pointer width")]
    RawPointerOutOfRange {
        /// The rejected zero-extended pointer word.
        value: u64,
    },
    /// The adapter rejected the argument count it received.
    #[error("foreign adapter rejected its argument count")]
    AdapterBadArgumentCount,
    /// The adapter rejected at least one argument tag.
    #[error("foreign adapter rejected an argument tag")]
    AdapterBadArgumentTag,
    /// The adapter found an interior NUL while constructing a C string.
    #[error("foreign adapter rejected a CString with an interior NUL byte")]
    AdapterInteriorNul,
    /// The adapter could not produce a valid result.
    #[error("foreign adapter reported a malformed result")]
    AdapterMalformedResult,
    /// The adapter returned a status this runtime does not know.
    #[error("foreign adapter returned unknown status {0}")]
    UnknownAdapterStatus(u32),
    /// The adapter's result tag does not match the declared result type.
    #[error("foreign adapter returned tag {actual}, expected tag {expected}")]
    MalformedResultTag {
        /// The expected bridge tag.
        expected: u8,
        /// The returned bridge tag.
        actual: u8,
    },
    /// The adapter wrote nonzero bytes into the reserved bridge field.
    #[error("foreign adapter returned nonzero reserved bytes")]
    MalformedResultReserved,
    /// The adapter ABI cannot return the declared type safely.
    #[error("the foreign adapter ABI does not support result type {0:?}")]
    UnsupportedResultType(ForeignTypeSpec),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn foreign_type_tags_are_pinned() {
        let expected = [
            ForeignType::Void,
            ForeignType::I8,
            ForeignType::I16,
            ForeignType::I32,
            ForeignType::I64,
            ForeignType::U8,
            ForeignType::U16,
            ForeignType::U32,
            ForeignType::U64,
            ForeignType::Bool,
            ForeignType::F32,
            ForeignType::F64,
            ForeignType::RawPtr,
            ForeignType::CString,
        ];
        for (tag, foreign_type) in expected.into_iter().enumerate() {
            let tag = tag as u8;
            assert_eq!(foreign_type.tag(), tag);
            assert_eq!(ForeignType::from_tag(tag), Some(foreign_type));
        }
        assert_eq!(ForeignType::from_tag(14), None);
    }

    #[test]
    fn foreign_abi_tag_is_pinned() {
        assert_eq!(ForeignAbi::C.tag(), 0);
        assert_eq!(ForeignAbi::from_tag(0), Some(ForeignAbi::C));
        assert_eq!(ForeignAbi::from_tag(1), None);
    }

    #[test]
    fn adapter_marker_and_status_values_are_pinned() {
        assert_eq!(FOREIGN_ADAPTER_ABI_VERSION, 2);
        assert_eq!(
            FOREIGN_ADAPTER_ABI_MARKER,
            "kira_foreign_adapter_abi_version_2"
        );
        assert_eq!(ForeignAdapterStatus::SUCCESS.0, 0);
        assert_eq!(ForeignAdapterStatus::BAD_ARGUMENT_COUNT.0, 1);
        assert_eq!(ForeignAdapterStatus::BAD_ARGUMENT_TAG.0, 2);
        assert_eq!(ForeignAdapterStatus::INTERIOR_NUL.0, 3);
        assert_eq!(ForeignAdapterStatus::MALFORMED_RESULT.0, 4);
        assert_eq!(ForeignAdapterStatus::BAD_RESULT_SLOT.0, 5);
    }

    #[test]
    fn the_aggregate_spec_tag_sits_past_every_scalar_tag() {
        // A scalar position keeps the scalar's own pinned tag, so no existing
        // byte moves; the aggregate byte is the first one past them.
        for tag in 0..=13u8 {
            let ty = ForeignType::from_tag(tag).expect("a pinned scalar tag");
            assert_eq!(ForeignTypeSpec::Scalar(ty).tag(), tag);
        }
        assert_eq!(ForeignTypeSpec::AGGREGATE_TAG, 14);
        assert_eq!(ForeignType::from_tag(ForeignTypeSpec::AGGREGATE_TAG), None);
        assert_eq!(
            ForeignTypeSpec::Aggregate(ForeignAggregateId(7)).tag(),
            ForeignTypeSpec::AGGREGATE_TAG
        );
        assert_eq!(
            ForeignTypeSpec::Aggregate(ForeignAggregateId(7)).bridge_tag(),
            BridgeValueTag::AGGREGATE
        );
    }

    #[test]
    fn a_signature_reports_whether_any_position_is_an_aggregate() {
        let scalars = ForeignSignature::new(
            [
                ForeignTypeSpec::Scalar(ForeignType::I32),
                ForeignTypeSpec::Scalar(ForeignType::F64),
            ],
            ForeignType::Void,
        );
        assert!(!scalars.has_aggregate());
        let in_param = ForeignSignature::new(
            [ForeignTypeSpec::Aggregate(ForeignAggregateId(0))],
            ForeignType::Void,
        );
        assert!(in_param.has_aggregate());
        let in_result = ForeignSignature::new(
            [ForeignTypeSpec::Scalar(ForeignType::I32)],
            ForeignTypeSpec::Aggregate(ForeignAggregateId(0)),
        );
        assert!(in_result.has_aggregate());
    }
}

/// One Kira function reachable from C as a function pointer.
///
/// A `@FFI.Callback`-typed value is the address of a generated entry thunk, and
/// this row is what the thunk was generated from: which function it enters and
/// the exact-width C signature it is entered with. A program's rows are indexed
/// by the id the frontend assigned, so the backend that emits the thunks and the
/// host that resolves one by name agree without either inspecting the other.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignCallback {
    /// The function index the thunk calls.
    function: u32,
    /// The C signature the thunk is entered with.
    signature: ForeignSignature,
}

impl ForeignCallback {
    /// Records `function` as callable from C through `signature`.
    pub fn new(function: u32, signature: ForeignSignature) -> ForeignCallback {
        ForeignCallback {
            function,
            signature,
        }
    }

    /// The function index the thunk calls.
    pub fn function(&self) -> u32 {
        self.function
    }

    /// The C signature the thunk is entered with.
    pub fn signature(&self) -> &ForeignSignature {
        &self.signature
    }
}
