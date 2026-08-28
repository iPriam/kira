//! Deep copy, equality, and release support for values lowered by LLVM.
//!
//! The implementation is divided by operation so each module stays small while
//! all methods remain inherent on [`super::Codegen`]. Keeping the methods on the
//! same type preserves the codegen API and lets the operation-specific walks
//! share the module's runtime declarations and target layout.

mod compare;
mod copy;
mod release;
mod support;
