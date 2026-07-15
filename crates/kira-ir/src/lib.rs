//! Kira mid-level and low-level IR, lowered from the HIR for the backends.
//!
//! Layer 3 of the Kira package graph.
//!
//! Design pending. The IR — programs, functions, constructs, ownership
//! modes, and the verified-program contract the backends consume — is designed
//! fresh, with model types following the index/arena pattern (no lifetimes).
