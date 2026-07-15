//! Kira syntax model: the token set and syntax tree the parser produces.
//!
//! Layer 1 of the Kira package graph.
//!
//! Design pending. The fresh parser defines its own token kinds and an
//! error-resilient syntax tree derived from the language corpus. Every node
//! carries spans, so the language server and the compiler consume one
//! frontend; model types follow the index/arena pattern with no lifetimes.
