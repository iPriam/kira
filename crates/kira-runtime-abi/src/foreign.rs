//! The safe Rust vocabulary and C ABI for calls through generated foreign adapters.

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

/// The complete type signature of one foreign import.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ForeignSignature {
    parameters: Box<[ForeignType]>,
    result: ForeignType,
}

impl ForeignSignature {
    /// Creates a signature from its parameter and result types.
    pub fn new(parameters: impl Into<Box<[ForeignType]>>, result: ForeignType) -> Self {
        Self {
            parameters: parameters.into(),
            result,
        }
    }

    /// Returns the parameter types in declaration order.
    pub fn parameters(&self) -> &[ForeignType] {
        &self.parameters
    }

    /// Returns the result type.
    pub const fn result(&self) -> ForeignType {
        self.result
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
}

impl ForeignArg<'_> {
    /// Returns this argument's exact foreign type.
    pub const fn foreign_type(self) -> ForeignType {
        match self {
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
        }
    }
}

/// An owned value returned from a foreign call.
///
/// `CString` is intentionally absent: returned C-string ownership is not part
/// of adapter ABI version 1.
#[derive(Debug, Clone, Copy, PartialEq)]
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
}

impl ForeignResult {
    /// Returns this result's exact foreign type.
    pub const fn foreign_type(self) -> ForeignType {
        match self {
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
        }
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
}

/// The version of the generated foreign-adapter ABI.
pub const FOREIGN_ADAPTER_ABI_VERSION: u32 = 1;

/// The versioned marker symbol every generated adapter library must export.
pub const FOREIGN_ADAPTER_ABI_MARKER: &str = "kira_foreign_adapter_abi_version_1";

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
/// # Safety
/// `args` must address `count` readable [`BridgeValue`]s, or be null when the
/// count is zero. `out` must address one writable [`BridgeValue`].
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
        /// The signature's type.
        expected: ForeignType,
        /// The supplied value's type.
        actual: ForeignType,
    },
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
    /// Adapter ABI version 1 cannot return the declared type safely.
    #[error("foreign adapter ABI version 1 does not support result type {0:?}")]
    UnsupportedResultType(ForeignType),
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
        assert_eq!(FOREIGN_ADAPTER_ABI_VERSION, 1);
        assert_eq!(
            FOREIGN_ADAPTER_ABI_MARKER,
            "kira_foreign_adapter_abi_version_1"
        );
        assert_eq!(ForeignAdapterStatus::SUCCESS.0, 0);
        assert_eq!(ForeignAdapterStatus::BAD_ARGUMENT_COUNT.0, 1);
        assert_eq!(ForeignAdapterStatus::BAD_ARGUMENT_TAG.0, 2);
        assert_eq!(ForeignAdapterStatus::INTERIOR_NUL.0, 3);
        assert_eq!(ForeignAdapterStatus::MALFORMED_RESULT.0, 4);
    }
}
