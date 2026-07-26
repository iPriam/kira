//! Shader IR lowered from KSL, consumed by the shader-language backends.
//!
//! Layer 3 of the Kira package graph.
//!
//! The IR is the checked module plus the two decisions no backend may make on
//! its own: which binding slot each resource takes on each target, and which
//! location each interface field takes. Both are decided once, here, so the
//! reflection a graphics host binds against and the source a backend emits
//! cannot disagree.
//!
//! Statements and expressions are not restated: a backend walks the checked
//! module's arenas directly. Copying that tree into a second nearly identical
//! one would buy nothing and give two places for a lowering bug to hide.

pub mod layout;
pub mod lower;
pub mod reflection;

use kira_ksl_semantics::model::CheckedModule;
use kira_shader_model::Reflection;

pub use lower::{entry_name, lower, type_name};
pub use reflection::{MAGIC, ReflectionError, decode, encode};

/// A shader ready for a backend to emit.
#[derive(Debug, Clone, PartialEq)]
pub struct ShaderIr {
    /// The checked module, whose arenas hold every body.
    pub module: CheckedModule,
    /// The reflection, absent when the file declared no shader.
    pub reflection: Option<Reflection>,
}

impl ShaderIr {
    /// The shader's reflection rendered as a `KSLR1` document.
    #[must_use]
    pub fn reflection_text(&self) -> String {
        self.reflection.as_ref().map(encode).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;
