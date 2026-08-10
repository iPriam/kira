//! The libclang handles that cross the C boundary by value, and the open C
//! enums that classify them.
//!
//! Nothing here calls anything: these are the shapes `clang-c/Index.h` passes
//! in registers, plus the discriminants Kira reads off them. The layout test at
//! the bottom is the contract — a reordered or repacked field would corrupt
//! every call in [`super::table`].

use std::ffi::{c_int, c_uint, c_void};

/// An opaque libclang index handle (`CXIndex`).
pub type CxIndex = *mut c_void;

/// An opaque libclang translation-unit handle (`CXTranslationUnit`).
pub type CxTranslationUnit = *mut c_void;

/// An opaque libclang file handle (`CXFile`).
pub type CxFile = *mut c_void;

/// An opaque libclang diagnostic handle (`CXDiagnostic`).
pub type CxDiagnostic = *mut c_void;

/// The client pointer threaded through a cursor visit (`CXClientData`).
pub type CxClientData = *mut c_void;

/// A libclang-owned string (`CXString`), disposed by its owner.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CxString {
    /// The libclang-owned payload; never interpreted here.
    pub data: *const c_void,
    /// libclang's private disposal flags.
    pub private_flags: c_uint,
}

/// A cursor into the parsed AST (`CXCursor`), passed and returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CxCursor {
    /// The cursor's kind, as an open C enum.
    pub kind: c_int,
    /// libclang's private discriminator.
    pub xdata: c_int,
    /// libclang's private payload.
    pub data: [*const c_void; 3],
}

/// A type in the parsed AST (`CXType`), passed and returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CxType {
    /// The type's kind, as an open C enum.
    pub kind: c_int,
    /// libclang's private payload.
    pub data: [*mut c_void; 2],
}

/// A source location (`CXSourceLocation`), passed and returned by value.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CxSourceLocation {
    /// libclang's private payload.
    pub ptr_data: [*const c_void; 2],
    /// libclang's private offset.
    pub int_data: c_uint,
}

/// What a cursor declares (`CXCursorKind`).
///
/// A transparent newtype rather than a Rust `enum`: libclang is free to hand
/// back a kind this build has never heard of, and a discriminant outside a
/// Rust `enum`'s set is undefined behavior.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorKind(pub c_int);

impl CursorKind {
    /// `CXCursor_StructDecl`.
    pub const STRUCT_DECL: Self = Self(2);
    /// `CXCursor_UnionDecl`.
    pub const UNION_DECL: Self = Self(3);
    /// `CXCursor_EnumDecl`.
    pub const ENUM_DECL: Self = Self(5);
    /// `CXCursor_FieldDecl`.
    pub const FIELD_DECL: Self = Self(6);
    /// `CXCursor_EnumConstantDecl`.
    pub const ENUM_CONSTANT_DECL: Self = Self(7);
    /// `CXCursor_FunctionDecl`.
    pub const FUNCTION_DECL: Self = Self(8);
    /// `CXCursor_VarDecl`.
    pub const VAR_DECL: Self = Self(9);
    /// `CXCursor_ParmDecl`.
    pub const PARM_DECL: Self = Self(10);
    /// `CXCursor_TypedefDecl`.
    pub const TYPEDEF_DECL: Self = Self(20);
    /// `CXCursor_InvalidFile`, the kind a null cursor carries.
    pub const INVALID_FILE: Self = Self(70);
}

/// What a type is (`CXTypeKind`).
///
/// A transparent newtype for the same reason [`CursorKind`] is.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TypeKind(pub c_int);

impl TypeKind {
    /// `CXType_Invalid`.
    pub const INVALID: Self = Self(0);
    /// `CXType_Unexposed` — a type libclang declines to describe.
    pub const UNEXPOSED: Self = Self(1);
    /// `CXType_Void`.
    pub const VOID: Self = Self(2);
    /// `CXType_Bool`.
    pub const BOOL: Self = Self(3);
    /// `CXType_Char_U` — plain `char` where it is unsigned.
    pub const CHAR_U: Self = Self(4);
    /// `CXType_UChar`.
    pub const UCHAR: Self = Self(5);
    /// `CXType_UShort`.
    pub const USHORT: Self = Self(8);
    /// `CXType_UInt`.
    pub const UINT: Self = Self(9);
    /// `CXType_ULong`.
    pub const ULONG: Self = Self(10);
    /// `CXType_ULongLong`.
    pub const ULONGLONG: Self = Self(11);
    /// `CXType_Char_S` — plain `char` where it is signed.
    pub const CHAR_S: Self = Self(13);
    /// `CXType_SChar`.
    pub const SCHAR: Self = Self(14);
    /// `CXType_Short`.
    pub const SHORT: Self = Self(16);
    /// `CXType_Int`.
    pub const INT: Self = Self(17);
    /// `CXType_Long`.
    pub const LONG: Self = Self(18);
    /// `CXType_LongLong`.
    pub const LONGLONG: Self = Self(19);
    /// `CXType_Float`.
    pub const FLOAT: Self = Self(21);
    /// `CXType_Double`.
    pub const DOUBLE: Self = Self(22);
    /// `CXType_LongDouble`.
    pub const LONGDOUBLE: Self = Self(23);
    /// `CXType_Pointer`.
    pub const POINTER: Self = Self(101);
    /// `CXType_Record`.
    pub const RECORD: Self = Self(105);
    /// `CXType_Enum`.
    pub const ENUM: Self = Self(106);
    /// `CXType_Typedef`.
    pub const TYPEDEF: Self = Self(107);
    /// `CXType_FunctionNoProto`.
    pub const FUNCTION_NO_PROTO: Self = Self(110);
    /// `CXType_FunctionProto`.
    pub const FUNCTION_PROTO: Self = Self(111);
    /// `CXType_ConstantArray`.
    pub const CONSTANT_ARRAY: Self = Self(112);
    /// `CXType_IncompleteArray`.
    pub const INCOMPLETE_ARRAY: Self = Self(114);
    /// `CXType_Elaborated` — `struct X` written where `X` would do.
    pub const ELABORATED: Self = Self(119);
}

/// How severe a parse diagnostic is (`CXDiagnosticSeverity`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct DiagnosticSeverity(pub c_int);

impl DiagnosticSeverity {
    /// `CXDiagnostic_Error` — the lowest severity that stops generation.
    pub const ERROR: Self = Self(3);
}

/// What a cursor visitor asks the walk to do next (`CXChildVisitResult`).
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct ChildVisitResult(pub c_int);

impl ChildVisitResult {
    /// `CXChildVisit_Continue` — visit the next sibling, not this child's own.
    pub const CONTINUE: Self = Self(1);
}

/// The visitor libclang calls once per child cursor.
pub type CxCursorVisitor =
    unsafe extern "C" fn(CxCursor, CxCursor, CxClientData) -> ChildVisitResult;

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    /// The by-value handles are what libclang's ABI passes in registers, so
    /// their field offsets are the contract rather than an implementation
    /// detail: a reordered or repacked field would corrupt every call.
    #[test]
    fn the_by_value_handles_have_the_layout_the_c_header_declares() {
        let pointer = size_of::<*const c_void>();
        assert_eq!(offset_of!(CxCursor, kind), 0);
        assert_eq!(offset_of!(CxCursor, xdata), size_of::<c_int>());
        assert_eq!(offset_of!(CxCursor, data), pointer);
        assert_eq!(size_of::<CxCursor>(), pointer * 4);

        assert_eq!(offset_of!(CxType, kind), 0);
        assert_eq!(offset_of!(CxType, data), pointer);
        assert_eq!(size_of::<CxType>(), pointer * 3);

        assert_eq!(offset_of!(CxString, data), 0);
        assert_eq!(offset_of!(CxString, private_flags), pointer);
        assert_eq!(size_of::<CxString>(), pointer * 2);
    }
}
