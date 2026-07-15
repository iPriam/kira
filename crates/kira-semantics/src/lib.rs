//! Semantic analyzer: type checking, ownership, and import resolution over the syntax tree.
//!
//! Layer 2 of the Kira package graph.
//!
//! Design pending. Semantic analysis runs as salsa queries over the frontend,
//! producing the HIR plus diagnostics. Ownership follows Rust-style affine
//! rules; the analyzer never bails on the first error.
