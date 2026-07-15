//! Hybrid runtime: loads hybrid modules, binds symbols, and hot-swaps modules in place.
//!
//! Layer 4 of the Kira package graph.
//!
//! Design pending. The hybrid execution model (bytecode plus native entry
//! points, with in-place module swap for live reload) is designed fresh with
//! the new runtime.
