//! The build system: orchestrates the frontend, backends, and packaging.
//!
//! Layer 7 of the Kira package graph.
//!
//! Design pending. Drives a source tree through frontend, IR, backend
//! selection, and artifact packaging, including managed toolchain fetch. FFI
//! autobind (generating Kira bindings for native libraries) is critical path
//! for KG/kira-graphics, not tail work, and is designed in here.
