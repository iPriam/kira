//! Rendering the generated `src/lib.rs`, in two halves.
//!
//! [`model`] decides what the crate says and refuses anything that cannot be
//! said; [`emit`] writes it out. The split is the file-size ladder's, taken at
//! the seam the two already had rather than at a line count.
//!
//! Both VM-family engines — the VM engine and the hybrid engine — are rendered
//! through here, with [`model::EngineBinding`] naming the difference. That is a
//! parity decision: the feature's central claim is that a consumer's code does
//! not change when the engine does, and two renderers would let the two APIs
//! drift apart one convenience at a time with no test noticing.

pub(crate) mod emit;
pub(crate) mod model;

pub(crate) use emit::lib_rs;
pub(crate) use model::{ClassModel, EngineBinding, FnModel, HOST_PARAM, Model, ParamModel};
