//! Semantic model (HIR): programs, constructs, symbols, types, scopes, and FFI declarations.
//!
//! Layer 2 of the Kira package graph.
//!
//! Design pending. The HIR is designed fresh for a salsa-query frontend: model
//! types carry no lifetimes (ids into arenas, interned names), so analysis has
//! no hidden global state and the language server and compiler share it.
