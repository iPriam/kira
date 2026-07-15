//! Native-to-VM bridge: runtime invokers and closure lifecycle plumbing.
//!
//! Layer 4 of the Kira package graph.
//!
//! Design pending. The host interface between native code and the VM is
//! designed fresh alongside the new runtime. This is native-only, feature-gated
//! surface that lives outside the portable VM core.
