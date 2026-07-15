//! Template-driven project generation.
//!
//! Ported from kira-zig `kira_app_generation/src/generator.zig`. The copy
//! machinery lands with the port; `TemplateKind` is the stable surface.

/// Which template `kira new` instantiates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateKind {
    App,
    Library,
}

impl TemplateKind {
    /// The directory name of the template under the templates root.
    pub fn template_dir_name(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Library => "library",
        }
    }
}
