//! Native libraries whose declared paths have been located, and the link inputs
//! a backend builds a command line from.
//!
//! A [`NativeLibrarySpec`](crate::NativeLibrarySpec) resolves into a
//! [`ResolvedNativeLibrary`]: one [`ResolvedTargetRow`] per declared target,
//! each carrying the located library file (when the row names one) plus the
//! [`NativeLinkAttributes`] — frameworks, system libraries, and flags — the
//! link line needs alongside it. A build gathers the rows its imports selected
//! into a [`NativeLinkInputs`], which is what every link path consumes.

use std::path::{Path, PathBuf};

use crate::spec::{Availability, LinkMode};
use crate::triple::TargetTriple;

/// The non-file link inputs a target row contributes.
///
/// A declared target is not always an archive path: a macOS row may name only
/// Apple frameworks, a Linux row only system libraries, and a wasm row only
/// emscripten flags. These travel with the row so the link line can be built
/// from the same selection that chose the archive.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLinkAttributes {
    /// Apple frameworks to link (`-framework <name>`).
    pub frameworks: Vec<String>,
    /// System libraries to link (`-l<name>`).
    pub system_libs: Vec<String>,
    /// Flags passed to the C compiler driver when compiling this library.
    pub compiler_flags: Vec<String>,
    /// Flags passed to the linker driver when linking this library.
    pub linker_flags: Vec<String>,
}

impl NativeLinkAttributes {
    /// True when the row contributes no link input of any kind.
    pub fn is_empty(&self) -> bool {
        self.frameworks.is_empty()
            && self.system_libs.is_empty()
            && self.compiler_flags.is_empty()
            && self.linker_flags.is_empty()
    }
}

/// One target row whose library file has been located on a concrete base
/// directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTargetRow {
    triple: TargetTriple,
    artifact: Option<PathBuf>,
    attributes: NativeLinkAttributes,
}

impl ResolvedTargetRow {
    /// Builds a resolved row from its triple, located artifact, and attributes.
    ///
    /// `artifact` is `None` for a row that names no library file of its own —
    /// the frameworks-only and system-libraries-only cases.
    pub fn new(
        triple: TargetTriple,
        artifact: Option<PathBuf>,
        attributes: NativeLinkAttributes,
    ) -> Self {
        Self {
            triple,
            artifact,
            attributes,
        }
    }

    /// The target this row provides link inputs for.
    pub fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// The located library file, when the row names one.
    pub fn artifact(&self) -> Option<&Path> {
        self.artifact.as_deref()
    }

    /// The frameworks, system libraries, and flags this row contributes.
    pub fn attributes(&self) -> &NativeLinkAttributes {
        &self.attributes
    }
}

/// A native library whose per-target rows have all been located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedNativeLibrary {
    name: String,
    link_mode: LinkMode,
    availability: Availability,
    targets: Vec<ResolvedTargetRow>,
}

impl ResolvedNativeLibrary {
    /// Builds a resolved library from its name, link mode, and located rows.
    pub fn new(
        name: impl Into<String>,
        link_mode: LinkMode,
        availability: Availability,
        targets: Vec<ResolvedTargetRow>,
    ) -> Self {
        Self {
            name: name.into(),
            link_mode,
            availability,
            targets,
        }
    }

    /// Whether the program can be built for a target this library omits.
    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// Whether the library is absent on `target` and the program said it may
    /// be, so calls into it there trap rather than linking.
    pub fn is_excluded_on(&self, target: &TargetTriple) -> bool {
        self.availability == Availability::Optional
            && !self.targets.iter().any(|row| row.triple() == target)
    }

    /// How the library reaches the program.
    pub fn link_mode(&self) -> LinkMode {
        self.link_mode
    }

    /// Whether the runtime finds this library rather than the linker.
    pub fn is_runtime_loaded(&self) -> bool {
        self.link_mode == LinkMode::Runtime
    }

    /// The library name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The located target rows.
    pub fn targets(&self) -> &[ResolvedTargetRow] {
        &self.targets
    }
}

/// Everything a link command needs to satisfy a build's foreign imports.
///
/// Accumulated from the target rows the build's imports selected, in first-use
/// order and without duplicates: a linker is order-sensitive, and two imports
/// naming the same library must not put its archive on the line twice.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeLinkInputs {
    archives: Vec<PathBuf>,
    frameworks: Vec<String>,
    system_libs: Vec<String>,
    compiler_flags: Vec<String>,
    linker_flags: Vec<String>,
    unavailable_imports: Vec<usize>,
}

impl NativeLinkInputs {
    /// Records that import `index` names a library absent on this target.
    ///
    /// Its adapter reports the absence instead of calling a symbol that would
    /// not link, so the import contributes nothing here.
    pub fn mark_unavailable(&mut self, index: usize) {
        if !self.unavailable_imports.contains(&index) {
            self.unavailable_imports.push(index);
        }
    }

    /// The imports whose library is absent on this target.
    pub fn unavailable_imports(&self) -> &[usize] {
        &self.unavailable_imports
    }

    /// The inputs of a build that links nothing foreign.
    ///
    /// A `const` rather than [`Default::default`] so a caller with no foreign
    /// imports can hand out a `&'static` reference instead of building an empty
    /// value it then has to keep alive.
    pub const EMPTY: Self = Self {
        archives: Vec::new(),
        frameworks: Vec::new(),
        system_libs: Vec::new(),
        compiler_flags: Vec::new(),
        linker_flags: Vec::new(),
        unavailable_imports: Vec::new(),
    };

    /// Adds one selected row's artifact and attributes, skipping repeats.
    pub fn push_row(&mut self, row: &ResolvedTargetRow) {
        if let Some(artifact) = row.artifact() {
            push_unique(&mut self.archives, artifact.to_path_buf());
        }
        let attributes = row.attributes();
        for framework in &attributes.frameworks {
            push_unique(&mut self.frameworks, framework.clone());
        }
        for library in &attributes.system_libs {
            push_unique(&mut self.system_libs, library.clone());
        }
        for flag in &attributes.compiler_flags {
            push_unique(&mut self.compiler_flags, flag.clone());
        }
        for flag in &attributes.linker_flags {
            push_unique(&mut self.linker_flags, flag.clone());
        }
    }

    /// True when no import selected any link input.
    pub fn is_empty(&self) -> bool {
        self.archives.is_empty()
            && self.frameworks.is_empty()
            && self.system_libs.is_empty()
            && self.compiler_flags.is_empty()
            && self.linker_flags.is_empty()
    }

    /// The selected library files, in first-use order.
    pub fn archives(&self) -> &[PathBuf] {
        &self.archives
    }

    /// The selected Apple frameworks, in first-use order.
    pub fn frameworks(&self) -> &[String] {
        &self.frameworks
    }

    /// The selected system libraries, in first-use order.
    pub fn system_libs(&self) -> &[String] {
        &self.system_libs
    }

    /// The selected C compiler flags, in first-use order.
    pub fn compiler_flags(&self) -> &[String] {
        &self.compiler_flags
    }

    /// The selected linker flags, in first-use order.
    pub fn linker_flags(&self) -> &[String] {
        &self.linker_flags
    }

    /// The driver arguments the attributes expand to, after the archives.
    ///
    /// `-framework <name>` is two arguments by design (the linker takes the
    /// name separately), a system library becomes `-l<name>`, and a linker flag
    /// is passed through untouched.
    pub fn driver_arguments(&self) -> Vec<String> {
        let mut arguments = Vec::new();
        for framework in &self.frameworks {
            arguments.push("-framework".to_owned());
            arguments.push(framework.clone());
        }
        for library in &self.system_libs {
            arguments.push(format!("-l{library}"));
        }
        arguments.extend(self.linker_flags.iter().cloned());
        arguments
    }
}

/// Appends `value` unless the list already holds it.
fn push_unique<T: PartialEq>(list: &mut Vec<T>, value: T) {
    if !list.contains(&value) {
        list.push(value);
    }
}

/// Why a native-library declaration could not be validated, resolved, or
/// cataloged.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NativeLibraryError {
    /// A target row named neither a library file nor any link input.
    #[error(
        "native library `{library}` target `{triple}` names no library, framework, \
         system library, or linker flag"
    )]
    PathlessRow {
        /// The library whose row is empty.
        library: String,
        /// The target of the empty row.
        triple: TargetTriple,
    },
    /// Two rows named the same target.
    #[error("native library `{library}` declares target `{triple}` more than once")]
    DuplicateTarget {
        /// The library with the duplicate target.
        library: String,
        /// The repeated target.
        triple: TargetTriple,
    },
    /// A declaration carried no library name.
    #[error("a native library declaration has no name")]
    NamelessLibrary,
    /// A row's library file was not present where it resolved to.
    #[error("native library `{library}` target `{triple}` is missing its archive at `{}`", path.display())]
    MissingArchive {
        /// The library whose file is missing.
        library: String,
        /// The target whose file is missing.
        triple: TargetTriple,
        /// Where the file was expected.
        path: PathBuf,
    },
    /// Two libraries in one catalog shared a name.
    #[error("native library `{library}` is declared more than once")]
    DuplicateLibrary {
        /// The repeated library name.
        library: String,
    },
    /// The catalog could not intern another distinct library name.
    #[error("too many distinct native-library names to intern")]
    NameSpaceExhausted,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    fn row(artifact: Option<&str>, attributes: NativeLinkAttributes) -> ResolvedTargetRow {
        ResolvedTargetRow::new(
            triple("aarch64-macos-none"),
            artifact.map(PathBuf::from),
            attributes,
        )
    }

    #[test]
    fn link_inputs_keep_first_use_order_and_drop_repeats() {
        let attributes = NativeLinkAttributes {
            frameworks: vec!["Metal".to_owned(), "AppKit".to_owned()],
            system_libs: vec!["objc".to_owned()],
            ..NativeLinkAttributes::default()
        };
        let mut inputs = NativeLinkInputs::default();
        inputs.push_row(&row(Some("/pkg/libsokol.a"), attributes.clone()));
        inputs.push_row(&row(Some("/pkg/libsokol.a"), attributes));

        assert_eq!(inputs.archives(), [PathBuf::from("/pkg/libsokol.a")]);
        assert_eq!(inputs.frameworks(), ["Metal", "AppKit"]);
        assert_eq!(inputs.system_libs(), ["objc"]);
    }

    #[test]
    fn a_row_with_no_artifact_still_contributes_its_attributes() {
        // The `kira_metal` shape: no archive at all, frameworks only.
        let mut inputs = NativeLinkInputs::default();
        inputs.push_row(&row(
            None,
            NativeLinkAttributes {
                frameworks: vec!["QuartzCore".to_owned()],
                ..NativeLinkAttributes::default()
            },
        ));
        assert!(inputs.archives().is_empty());
        assert!(!inputs.is_empty());
        assert_eq!(
            inputs.driver_arguments(),
            ["-framework".to_owned(), "QuartzCore".to_owned()]
        );
    }

    #[test]
    fn driver_arguments_expand_every_attribute_kind() {
        let mut inputs = NativeLinkInputs::default();
        inputs.push_row(&row(
            None,
            NativeLinkAttributes {
                frameworks: vec!["Metal".to_owned()],
                system_libs: vec!["objc".to_owned(), "m".to_owned()],
                compiler_flags: vec!["--use-port=emdawnwebgpu".to_owned()],
                linker_flags: vec!["-sERROR_ON_UNDEFINED_SYMBOLS=0".to_owned()],
            },
        ));
        assert_eq!(
            inputs.driver_arguments(),
            [
                "-framework",
                "Metal",
                "-lobjc",
                "-lm",
                "-sERROR_ON_UNDEFINED_SYMBOLS=0",
            ]
        );
        // A compiler flag is not a link argument; it reaches the driver through
        // the compile step that consumes the library's own sources.
        assert_eq!(inputs.compiler_flags(), ["--use-port=emdawnwebgpu"]);
    }
}
