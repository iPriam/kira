//! The bodyless-declaration seam: turning an `@FFI.Extern` or `@FFI.Syscall`
//! into a validated [`HirForeign`] row, and type-checking a call to one.
//!
//! # Why the checks live here
//!
//! Whether a type can cross the C seam, whether an ABI is supported, and
//! whether the annotation block is well-formed all have the same answer on
//! every backend — the seam is a property of the declaration, not of the engine
//! that binds the symbol. Putting the checks above the backend split, beside
//! [`crate::exports`], is what keeps three engines from each growing their own
//! opinion of what a foreign call is.
//!
//! A refused declaration is never recorded: [`HirProgram::foreign`] only ever
//! holds signatures the frontend accepted, so a backend binds against a contract
//! it can trust. A call resolves to [`Callee::Foreign`] by name, exactly as a
//! user call resolves to [`Callee::User`], and the argument coercion `String ->
//! CString` is the one implicit conversion the seam allows.
//!
//! This file owns the C-symbol form and everything the two forms share. What a
//! system call additionally requires — a name the compiler has a number for, a
//! target that can reach the Linux kernel, an argument list that fits in
//! registers — is [`crate::syscall`]'s.

use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType, ForeignTypeSpec};
use kira_semantics_model::hir::{Callee, ForeignId, HirExpr, HirExprId, HirForeign};
use kira_semantics_model::{FloatSpelling, IntSpelling, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{ForeignKind, ForeignMark, Function, Item, Param, TypeRef};

use crate::analyze::{Analyzer, FnCtx};
use crate::ffi_types::FfiStructKind;

/// Whether a `@FFI.*` struct kind has no meaning of its own in a parameter or
/// result position.
///
/// Only the inline array: C decays an array there to a pointer, which is a
/// different type with different ownership. A `@FFI.Callback` *is* a pointer,
/// and a `@FFI.Struct` crosses by value, so both are at home there.
fn is_deferred_ffi(kind: FfiStructKind) -> bool {
    matches!(kind, FfiStructKind::Array)
}

/// Whether a foreign type sits in a parameter or the result position.
///
/// The two positions differ on exactly two types: `Void` is a legal result but
/// not a legal parameter, and `CString` is a legal parameter but not a legal
/// result. Everything else maps the same way in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Position {
    /// A parameter of the foreign function.
    Param,
    /// The result of the foreign function.
    Result,
}

mod call;
mod collect;
mod shape;
mod types;

/// The seam type a written parameter or result crosses as, with the wrapper
/// struct to rebuild when it was a single-scalar-field handle struct.
struct ForeignSeam {
    /// The position the value crosses the seam as: a C-width scalar, or an index
    /// into the program's C-layout aggregate table.
    spec: ForeignTypeSpec,
    /// The Kira struct the value is read from and rebuilt into, or `None` for an
    /// ordinary scalar. Set for both struct shapes that cross: a
    /// single-scalar-field handle, whose wire position is its field's scalar,
    /// and a C-layout aggregate, whose wire position is its table index.
    wrapper: Option<StructId>,
    /// The C-layout struct this position points at, when it was written as an
    /// `@FFI.Pointer`. The wire position stays a pointer word; this is what lets
    /// a call pass the struct and have its address taken.
    pointee: Option<kira_semantics_model::hir::ForeignPointee>,
    /// The `distinct` type this position was written as, when it was one.
    ///
    /// The wire position is the representation, because that is what the type
    /// *is*; this is what keeps the Kira side of the call nominal, so a `TabId`
    /// parameter takes a `TabId` and refuses the `U32` underneath it.
    distinct: Option<Type>,
}

impl ForeignSeam {
    /// A scalar position with no Kira-side struct.
    fn scalar(ty: ForeignType) -> Self {
        Self {
            spec: ForeignTypeSpec::Scalar(ty),
            wrapper: None,
            pointee: None,
            distinct: None,
        }
    }
}

/// A mapped foreign signature: the wire types plus the Kira-side wrappers that
/// never reach the wire.
struct MappedForeign {
    /// The C-seam parameter and result types.
    signature: ForeignSignature,
    /// One wrapper per parameter, `Some` for a single-scalar-field handle.
    param_wrappers: Box<[Option<StructId>]>,
    /// One pointee per parameter, `Some` for an `@FFI.Pointer` to a C-layout
    /// struct.
    param_pointees: Box<[Option<kira_semantics_model::hir::ForeignPointee>]>,
    /// The result's pointer target, if it was written as an `@FFI.Pointer` to a
    /// C-layout struct.
    result_pointee: Option<StructId>,
    /// The result's wrapper, if it is a single-scalar-field handle.
    result_wrapper: Option<StructId>,
    /// One `distinct` type per parameter, `Some` when the parameter was written
    /// as one.
    param_distincts: Box<[Option<Type>]>,
    /// The result's `distinct` type, when it was written as one.
    result_distinct: Option<Type>,
}

/// The [`ForeignType`] a resolved type crosses as when it is already a seam
/// scalar, without emitting anything. Used to test a struct's sole field: a
/// handle struct's member is an integer, `Float`/`F32`, `Bool`, or `RawPtr`. A
/// `CString` (borrowed, never a stored field) or any aggregate returns `None`.
pub(crate) fn scalar_foreign_type(ty: Type) -> Option<ForeignType> {
    match ty {
        Type::Int(spelling) => Some(int_foreign_type(spelling)),
        Type::Float(FloatSpelling::F32) => Some(ForeignType::F32),
        Type::Float(FloatSpelling::Plain) => Some(ForeignType::F64),
        Type::Bool => Some(ForeignType::Bool),
        Type::RawPtr | Type::ForeignPtr(_) => Some(ForeignType::RawPtr),
        _ => None,
    }
}

/// The fixed-width [`ForeignType`] an integer spelling crosses as. Bare `Int`
/// is the 64-bit one, which is why it needs no separate spelling.
fn int_foreign_type(spelling: IntSpelling) -> ForeignType {
    match spelling {
        IntSpelling::Plain => ForeignType::I64,
        IntSpelling::I8 => ForeignType::I8,
        IntSpelling::I16 => ForeignType::I16,
        IntSpelling::I32 => ForeignType::I32,
        IntSpelling::U8 => ForeignType::U8,
        IntSpelling::U16 => ForeignType::U16,
        IntSpelling::U32 => ForeignType::U32,
        IntSpelling::U64 => ForeignType::U64,
    }
}

/// The Kira [`Type`] a foreign type maps back to — a call's result type, and
/// the type a non-`CString` argument must be assignable to.
///
/// `CString` maps to `String` here only for completeness: an argument to a
/// `CString` parameter is checked by the explicit `String -> CString` rule, not
/// through this map, and a `CString` never appears as a result.
fn kira_type_for_foreign(foreign_type: ForeignType) -> Type {
    match foreign_type {
        ForeignType::Void => Type::Void,
        ForeignType::I8 => Type::Int(IntSpelling::I8),
        ForeignType::I16 => Type::Int(IntSpelling::I16),
        ForeignType::I32 => Type::Int(IntSpelling::I32),
        ForeignType::I64 => Type::Int(IntSpelling::Plain),
        ForeignType::U8 => Type::Int(IntSpelling::U8),
        ForeignType::U16 => Type::Int(IntSpelling::U16),
        ForeignType::U32 => Type::Int(IntSpelling::U32),
        ForeignType::U64 => Type::Int(IntSpelling::U64),
        ForeignType::Bool => Type::Bool,
        ForeignType::F32 => Type::Float(FloatSpelling::F32),
        ForeignType::F64 => Type::Float(FloatSpelling::Plain),
        ForeignType::RawPtr => Type::RawPtr,
        ForeignType::CString => Type::String,
    }
}

/// The Kira [`Type`] a signature position maps back to.
///
/// An aggregate position has no scalar spelling and is unreachable here: the
/// declaration checks refuse an aggregate at the seam before a signature is
/// recorded, so no [`HirForeign`] holds one. It maps to [`Type::Error`] rather
/// than a guess, which is the type that stays silent downstream.
fn kira_type_for_spec(spec: ForeignTypeSpec) -> Type {
    match spec.scalar() {
        Some(ty) => kira_type_for_foreign(ty),
        None => Type::Error,
    }
}

/// Whether a Kira argument type is accepted for a foreign parameter position.
///
/// A `CString` parameter accepts a Kira `String` and nothing else — the single
/// explicit coercion. Every other parameter accepts a value assignable to the
/// Kira type it maps back to, so an integer literal (`Int`) reaches any fixed
/// width exactly as it does elsewhere.
fn foreign_arg_matches(actual: Type, param: ForeignTypeSpec) -> bool {
    match param.scalar() {
        Some(ForeignType::CString) => actual == Type::String,
        _ => actual.assignable_to(kira_type_for_spec(param)),
    }
}
