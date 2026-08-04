//! A minimal safe binding to `libclang`, the C parser Kira reads headers with.
//!
//! Layer 0 of the Kira package graph.
//!
//! # Why a binding rather than a subprocess
//!
//! `clang -Xclang -ast-dump=json` will print a header's AST, but it prints
//! *types* as the strings a C programmer writes: `char[7]`, `const char *`,
//! `void (*)(int, void *)`. A generator built on that output would have to
//! re-parse C declarator syntax to answer "how many elements" and "pointer to
//! what" — the two questions it exists to answer — and would get a different
//! answer than the compiler on the first header that nests them. libclang
//! answers both structurally, so this crate loads it.
//!
//! # Why it is loaded rather than linked
//!
//! `libclang` is a shared library in the managed LLVM bundle. Linking it would
//! make every `kira` binary refuse to start on a machine whose bundle is being
//! replaced, and would put an rpath into a binary that has no other reason for
//! one. Loading it on demand turns that into a diagnostic on the one command
//! that needs a C parser.
//!
//! # The unsafe fence
//!
//! Every `unsafe` block lives in [`api`] and in the two `Drop`s in [`unit`].
//! The handles libclang passes by value are `#[repr(C)]` there with a layout
//! test beside them, no handle can be constructed outside this crate, and each
//! wrapper borrows the unit that owns it — so a caller cannot hold a cursor
//! into a released translation unit.

mod api;
mod cursor;
mod unit;

pub use api::{CursorKind, LoadError, TypeKind};
pub use cursor::{CType, Cursor};
pub use unit::{Clang, ParseError, TranslationUnit};
