//! Parser producing the Kira syntax tree from tokens.
//!
//! Layer 1 of the Kira package graph.
//!
//! Design pending. The parser is hand-written and error-resilient: it always
//! produces a tree plus diagnostics and never bails on the first error, so the
//! language server and the compiler share one frontend. It runs as a salsa
//! query with no hidden global state.
