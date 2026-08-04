//! The raw libclang C surface: the structs passed by value, the open C enums,
//! and the symbol table loaded out of a `libclang` shared library.
//!
//! Everything `unsafe` in this crate is fenced here and in the two `Drop`s in
//! [`crate::unit`]. The rest of the crate is safe Rust over these calls.
//!
//! Only the subset the binding generator needs is declared. Adding a call means
//! adding its `extern "C"` signature in [`table`], copied from
//! `clang-c/Index.h`; a signature that disagrees with the header is undefined
//! behavior, so each one is written out in full rather than punned through a
//! generic pointer.

mod calls;
mod raw;
mod table;

pub use raw::{
    ChildVisitResult, CursorKind, CxClientData, CxCursor, CxIndex, CxTranslationUnit, CxType,
    TypeKind,
};
pub use table::{Api, LoadError};

pub(crate) use raw::DiagnosticSeverity;
