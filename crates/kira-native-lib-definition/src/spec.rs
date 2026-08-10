//! The declared shape of a package's native libraries, before any path is
//! located.
//!
//! A package declares a C library in one of two places: inline in its
//! `package.kira` (`let nativeLibraries = [NativeLibrary { ... }]`) or in a
//! `NativeLibs/<name>.toml`. Both spellings decode into the one model here —
//! a [`NativeLibrarySpec`] naming the library, how it links, the headers and
//! sources it is built from, the bindings it wants generated, and one
//! [`NativeTargetSpec`] per target triple.
//!
//! Nothing here touches the disk: [`NativeLibrarySpec::resolve`] locates each
//! row's file through an injected predicate, which is what keeps this crate a
//! pure model.

use std::collections::HashSet;
use std::path::Path;

use crate::resolved::{
    MissingArchive, NativeLibraryError, NativeLinkAttributes, ResolvedNativeLibrary,
    ResolvedTargetRow,
};
use crate::triple::TargetTriple;

/// How a declared library is linked into a program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LinkMode {
    /// Linked from a static archive at build time.
    #[default]
    Static,
    /// Resolved against a shared library.
    Dynamic,
    /// Found by the runtime on first call, with no link-time dependency.
    ///
    /// For a library that may simply not be present — a Vulkan or Direct3D
    /// driver on a machine that has neither — this is the only honest mode.
    /// Linking against it would make a program that cannot start on a machine
    /// missing the driver, and *checking* for it at build time would tie the
    /// binary to the machine that built it. So nothing is linked: the runtime
    /// opens the library and looks each symbol up the first time it is called.
    ///
    /// The declaration is all a program writes. Loading is the compiler's and
    /// the runtime's job, never the caller's — a program that opened libraries
    /// and read symbol pointers by hand would be doing what the toolchain
    /// exists to do.
    Runtime,
}

impl LinkMode {
    /// Reads the case name a declaration writes, in either spelling.
    ///
    /// `package.kira` writes `.Static` / `LinkMode.Static`; a `NativeLibs/*.toml`
    /// writes `"static"`. Both appear in the pinned corpus, so both are read.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Static" | "static" => Some(Self::Static),
            "Dynamic" | "dynamic" => Some(Self::Dynamic),
            "Runtime" | "runtime" => Some(Self::Runtime),
            _ => None,
        }
    }

    /// The lowercase label used in diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Static => "static",
            Self::Dynamic => "dynamic",
            Self::Runtime => "runtime",
        }
    }
}

/// Whether a program can be built for a target the library does not support.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Availability {
    /// Every target the build selects must be one this library declares.
    ///
    /// The default, and the right answer for a library the program cannot run
    /// without: a missing row is a mistake worth naming at build time rather
    /// than a failure at the first call.
    #[default]
    Required,
    /// The library may be absent on some targets, and the program says so.
    ///
    /// On a target it declares no row for, the library is *excluded*: nothing
    /// is linked and no archive is looked for. A call into it there traps,
    /// naming the library and symbol, instead of the build failing — which is
    /// what a Direct3D binding needs on macOS and a Vulkan one needs on a
    /// machine with no driver. The platform-specific code is still compiled and
    /// still type-checked; only its link-time dependency goes away.
    Optional,
}

impl Availability {
    /// Reads the case name a declaration writes, in either spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "Required" | "required" => Some(Self::Required),
            "Optional" | "optional" => Some(Self::Optional),
            _ => None,
        }
    }

    /// The lowercase label used in diagnostics.
    pub fn label(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
        }
    }
}

/// The C headers a library is bound and compiled against.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NativeHeaders {
    /// The single header that includes the rest, when the library names one.
    pub entrypoint: Option<String>,
    /// Include directories, relative to the declaring manifest.
    pub include_dirs: Vec<String>,
    /// Preprocessor defines applied on every target.
    pub defines: Vec<String>,
}

/// How much of a header a binding generator should expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AutobindMode {
    /// Bind every public declaration the headers expose.
    AllPublic,
    /// Bind only the functions and structs the declaration names.
    #[default]
    Selected,
}

impl AutobindMode {
    /// Reads the case name a declaration writes, in either spelling.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "AllPublic" | "all_public" => Some(Self::AllPublic),
            "Selected" | "selected" => Some(Self::Selected),
            _ => None,
        }
    }
}

/// A named binding-generator ruleset (`vulkan`, `directx12`, …).
///
/// Free-form on purpose: the profile names a generator's own ruleset, so a
/// profile this compiler has never heard of must survive being read rather than
/// fail a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutobindProfile(String);

impl AutobindProfile {
    /// Names a profile.
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    /// The profile name as written.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The bindings a library wants generated from its headers.
///
/// Carried, not yet acted on: binding generation is its own slice. Reading it
/// into the model is what keeps a declaration from being silently dropped.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AutobindSpec {
    /// The Kira module the generated bindings land in.
    pub module: Option<String>,
    /// The headers to generate from.
    pub headers: Vec<String>,
    /// Individually named functions to bind.
    pub functions: Vec<String>,
    /// Individually named structs to bind.
    pub structs: Vec<String>,
    /// How much of the headers to expose.
    pub mode: AutobindMode,
    /// The generator ruleset, when the declaration names one.
    pub profile: Option<AutobindProfile>,
    /// Where generated bindings are written, when the declaration names a path.
    pub output: Option<String>,
}

/// The library file a target row names, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NativeArtifact {
    /// A static archive at a path relative to the declaring manifest.
    StaticArchive(String),
    /// A shared library at a path relative to the declaring manifest.
    SharedLibrary(String),
    /// No library file of this row's own: its frameworks, system libraries, and
    /// flags are the whole contribution. This is both the `dynamicLib: ""` row,
    /// which leaves the loader to find the library by its install name, and the
    /// frameworks-only row that names no library at all.
    None,
}

impl NativeArtifact {
    /// The declared path, when the row names a file.
    ///
    /// A blank path is no path: it comes back `None` rather than as a name that
    /// would join to the base directory itself.
    pub fn path(&self) -> Option<&str> {
        match self {
            Self::StaticArchive(path) | Self::SharedLibrary(path) => {
                Some(path.as_str()).filter(|path| !path.trim().is_empty())
            }
            Self::None => None,
        }
    }

    /// A row's artifact from the two path spellings a declaration may use.
    ///
    /// An empty or whitespace-only path is [`NativeArtifact::None`]: the corpus
    /// writes `dynamicLib: ""` for a library resolved by install name, and
    /// treating that as a path would look for a file named nothing.
    pub fn from_paths(static_lib: Option<&str>, dynamic_lib: Option<&str>) -> Self {
        if let Some(path) = static_lib.filter(|path| !path.trim().is_empty()) {
            return Self::StaticArchive(path.to_owned());
        }
        if let Some(path) = dynamic_lib.filter(|path| !path.trim().is_empty()) {
            return Self::SharedLibrary(path.to_owned());
        }
        Self::None
    }
}

/// One target's declaration: the library file to link on it, plus the link
/// inputs that go beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTargetSpec {
    triple: TargetTriple,
    artifact: NativeArtifact,
    defines: Vec<String>,
    attributes: NativeLinkAttributes,
}

impl NativeTargetSpec {
    /// Declares a target row from its triple and library file.
    pub fn new(triple: TargetTriple, artifact: NativeArtifact) -> Self {
        Self {
            triple,
            artifact,
            defines: Vec::new(),
            attributes: NativeLinkAttributes::default(),
        }
    }

    /// Declares a static-archive row, the common case.
    pub fn static_archive(triple: TargetTriple, path: impl Into<String>) -> Self {
        Self::new(triple, NativeArtifact::StaticArchive(path.into()))
    }

    /// Adds the preprocessor defines this target compiles the library with.
    #[must_use]
    pub fn with_defines(mut self, defines: Vec<String>) -> Self {
        self.defines = defines;
        self
    }

    /// Adds the frameworks, system libraries, and flags this target links with.
    #[must_use]
    pub fn with_attributes(mut self, attributes: NativeLinkAttributes) -> Self {
        self.attributes = attributes;
        self
    }

    /// The target this row is for.
    pub fn triple(&self) -> &TargetTriple {
        &self.triple
    }

    /// The library file this row names, if any.
    pub fn artifact(&self) -> &NativeArtifact {
        &self.artifact
    }

    /// The preprocessor defines this target compiles the library with.
    pub fn defines(&self) -> &[String] {
        &self.defines
    }

    /// The frameworks, system libraries, and flags this row links with.
    pub fn attributes(&self) -> &NativeLinkAttributes {
        &self.attributes
    }
}

/// A native library as declared, before its files are located.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeLibrarySpec {
    name: String,
    link_mode: LinkMode,
    availability: Availability,
    headers: Option<NativeHeaders>,
    sources: Vec<String>,
    autobind: Option<AutobindSpec>,
    targets: Vec<NativeTargetSpec>,
}

impl NativeLibrarySpec {
    /// Declares and validates a library.
    ///
    /// Rejects a nameless library ([`NativeLibraryError::NamelessLibrary`]), a
    /// row that contributes nothing at all
    /// ([`NativeLibraryError::PathlessRow`]), and two rows naming the same
    /// target ([`NativeLibraryError::DuplicateTarget`]).
    pub fn new(
        name: impl Into<String>,
        link_mode: LinkMode,
        targets: Vec<NativeTargetSpec>,
    ) -> Result<Self, NativeLibraryError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(NativeLibraryError::NamelessLibrary);
        }
        let mut seen = HashSet::with_capacity(targets.len());
        for row in &targets {
            // A static row that names no archive and contributes no framework,
            // system library, or flag would put nothing on the link line, so it
            // can only be a declaration whose path was meant to be there. The
            // same row under `Dynamic` is the corpus's `dynamicLib: ""`: the
            // library is found by its own name at link or load time, so there
            // is nothing missing.
            if link_mode == LinkMode::Static
                && row.artifact.path().is_none()
                && row.attributes.is_empty()
                && row.defines.is_empty()
            {
                return Err(NativeLibraryError::PathlessRow {
                    library: name.clone(),
                    triple: row.triple.clone(),
                });
            }
            if !seen.insert(row.triple.clone()) {
                return Err(NativeLibraryError::DuplicateTarget {
                    library: name.clone(),
                    triple: row.triple.clone(),
                });
            }
        }
        Ok(Self {
            name,
            link_mode,
            availability: Availability::Required,
            headers: None,
            sources: Vec::new(),
            autobind: None,
            targets,
        })
    }

    /// Marks the library as one the program can be built without on a target
    /// it declares no row for.
    #[must_use]
    pub fn with_availability(mut self, availability: Availability) -> Self {
        self.availability = availability;
        self
    }

    /// Whether a target this library declares no row for is a build failure.
    pub fn availability(&self) -> Availability {
        self.availability
    }

    /// Adds the headers the library is bound and compiled against.
    #[must_use]
    pub fn with_headers(mut self, headers: NativeHeaders) -> Self {
        self.headers = Some(headers);
        self
    }

    /// Adds the C sources built into the library.
    #[must_use]
    pub fn with_sources(mut self, sources: Vec<String>) -> Self {
        self.sources = sources;
        self
    }

    /// Adds the bindings the library wants generated.
    #[must_use]
    pub fn with_autobind(mut self, autobind: AutobindSpec) -> Self {
        self.autobind = Some(autobind);
        self
    }

    /// The library name (the key a foreign import resolves against).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// How the library is linked.
    pub fn link_mode(&self) -> LinkMode {
        self.link_mode
    }

    /// The headers the library is bound and compiled against.
    pub fn headers(&self) -> Option<&NativeHeaders> {
        self.headers.as_ref()
    }

    /// The C sources built into the library, relative to its manifest.
    pub fn sources(&self) -> &[String] {
        &self.sources
    }

    /// The bindings the library wants generated.
    pub fn autobind(&self) -> Option<&AutobindSpec> {
        self.autobind.as_ref()
    }

    /// The declared target rows.
    pub fn targets(&self) -> &[NativeTargetSpec] {
        &self.targets
    }

    /// Locates each row's library file relative to `base_dir`, without touching
    /// the disk.
    ///
    /// Joins `base_dir` with each row's relative path (a pure join, no I/O),
    /// then asks the injected `exists` predicate whether the file is present. A
    /// row that names no file resolves to its attributes alone. The predicate
    /// is the seam the I/O layer fills with `|path| path.exists()`.
    ///
    /// # Only the row being built has to exist
    ///
    /// `wanted` is the target this build links for. Its archive is required and
    /// its absence is a [`NativeLibraryError::MissingArchive`]; every *other*
    /// row is located but not required. A cross-platform library legitimately
    /// declares every platform it supports while a given machine has archives
    /// for one — `kira-graphics` declares ten and a macOS checkout builds one —
    /// and requiring all of them would make such a library unusable everywhere.
    /// Passing `None` requires every row, which is what a caller checking a
    /// whole matrix wants.
    ///
    /// One row gains an attribute it did not declare: a dynamic library whose
    /// row names no file and carries no attributes at all links by its own name
    /// (`-l<name>`), which is what `dynamicLib: ""` means. A dynamic row that
    /// *does* name frameworks or system libraries is taken at its word — those
    /// are the symbols, and there is no library of the declaration's own name
    /// to find.
    pub fn resolve(
        &self,
        base_dir: &Path,
        wanted: Option<&TargetTriple>,
        exists: impl Fn(&Path) -> bool,
    ) -> Result<ResolvedNativeLibrary, NativeLibraryError> {
        let mut rows = Vec::with_capacity(self.targets.len());
        for row in &self.targets {
            // A runtime library is never linked, and an optional one may be
            // absent, so neither has an archive that can be missing.
            let required = self.link_mode != LinkMode::Runtime
                && self.availability != Availability::Optional
                && wanted.is_none_or(|wanted| *wanted == row.triple);
            let artifact = match row.artifact.path() {
                Some(relative) => {
                    let located = base_dir.join(relative);
                    if !exists(&located) {
                        if required {
                            return Err(NativeLibraryError::MissingArchive(Box::new(
                                MissingArchive {
                                    library: self.name.clone(),
                                    triple: row.triple.clone(),
                                    path: located,
                                },
                            )));
                        }
                        None
                    } else {
                        Some(located)
                    }
                }
                None => None,
            };
            let mut attributes = row.attributes.clone();
            if artifact.is_none() && attributes.is_empty() && self.link_mode == LinkMode::Dynamic {
                attributes.system_libs.push(self.name.clone());
            }
            // A runtime file is located exactly as an artifact is, and its
            // absence is reported on the same terms: a row for the target being
            // built must have the files its program will look for, and a row for
            // another platform is located but not required.
            let mut runtime_files = Vec::with_capacity(attributes.runtime_files.len());
            for declared in &attributes.runtime_files {
                let located = base_dir.join(declared);
                if exists(&located) {
                    runtime_files.push(located);
                } else if required {
                    return Err(NativeLibraryError::MissingArchive(Box::new(
                        MissingArchive {
                            library: self.name.clone(),
                            triple: row.triple.clone(),
                            path: located,
                        },
                    )));
                }
            }
            rows.push(
                ResolvedTargetRow::new(row.triple.clone(), artifact, attributes)
                    .with_runtime_files(runtime_files),
            );
        }
        Ok(ResolvedNativeLibrary::new(
            self.name.clone(),
            self.link_mode,
            self.availability,
            rows,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn triple(text: &str) -> TargetTriple {
        TargetTriple::parse(text).expect("a valid triple")
    }

    #[test]
    fn rejects_a_row_that_contributes_nothing() {
        let error = NativeLibrarySpec::new(
            "ffimath",
            LinkMode::Static,
            vec![NativeTargetSpec::static_archive(
                triple("aarch64-macos-none"),
                "   ",
            )],
        )
        .expect_err("an empty row is rejected");
        assert_eq!(
            error,
            NativeLibraryError::PathlessRow {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
            }
        );
    }

    /// The Dawn shape: an import library to link, and a directory of shared
    /// libraries the loader will want beside the program.
    #[test]
    fn runtime_files_are_located_against_the_manifest() {
        let wanted = triple("x86_64-windows-msvc");
        let spec = NativeLibrarySpec::new(
            "dawn",
            LinkMode::Dynamic,
            vec![
                NativeTargetSpec::new(
                    wanted.clone(),
                    NativeArtifact::SharedLibrary("lib/webgpu_dawn.lib".to_owned()),
                )
                .with_attributes(NativeLinkAttributes {
                    runtime_files: vec!["bin".to_owned()],
                    ..NativeLinkAttributes::default()
                }),
            ],
        )
        .expect("a dynamic row with an import library is valid");
        let resolved = spec
            .resolve(Path::new("/pkg"), Some(&wanted), |_| true)
            .expect("every declared path is present");
        assert_eq!(
            resolved.targets()[0].runtime_files(),
            [PathBuf::from("/pkg/bin")]
        );
    }

    /// A runtime file the target being built does not have is refused by name.
    ///
    /// Otherwise the program links clean and cannot start, and what the loader
    /// says about that names no file at all.
    #[test]
    fn a_missing_runtime_file_is_refused_for_the_target_being_built() {
        let wanted = triple("x86_64-windows-msvc");
        let spec = NativeLibrarySpec::new(
            "dawn",
            LinkMode::Dynamic,
            vec![
                NativeTargetSpec::new(wanted.clone(), NativeArtifact::None).with_attributes(
                    NativeLinkAttributes {
                        runtime_files: vec!["bin/webgpu_dawn.dll".to_owned()],
                        ..NativeLinkAttributes::default()
                    },
                ),
            ],
        )
        .expect("a dynamic row resolved by install name is valid");
        let error = spec
            .resolve(Path::new("/pkg"), Some(&wanted), |_| false)
            .expect_err("the declared file is not there");
        let NativeLibraryError::MissingArchive(missing) = error else {
            panic!("expected the missing-file refusal");
        };
        assert_eq!(missing.path, PathBuf::from("/pkg/bin/webgpu_dawn.dll"));
    }

    #[test]
    fn a_frameworks_only_row_is_a_valid_declaration() {
        // The `kira_metal` shape: no library file, frameworks and `-lobjc` only.
        let spec = NativeLibrarySpec::new(
            "kira_metal",
            LinkMode::Dynamic,
            vec![
                NativeTargetSpec::new(triple("aarch64-macos-none"), NativeArtifact::None)
                    .with_attributes(NativeLinkAttributes {
                        frameworks: vec!["Metal".to_owned()],
                        system_libs: vec!["objc".to_owned()],
                        ..NativeLinkAttributes::default()
                    }),
            ],
        )
        .expect("a frameworks-only declaration is valid");
        let resolved = spec
            .resolve(Path::new("/pkg"), None, |_| panic!("no path to check"))
            .expect("resolution touches no path");
        assert_eq!(resolved.targets()[0].artifact(), None);
        assert_eq!(resolved.targets()[0].attributes().frameworks, ["Metal"]);
    }

    #[test]
    fn an_empty_dynamic_lib_path_is_not_a_path() {
        // `dynamicLib: ""` means "find it by install name", not "the file
        // named empty string".
        assert_eq!(
            NativeArtifact::from_paths(None, Some("")),
            NativeArtifact::None
        );
        assert_eq!(
            NativeArtifact::from_paths(None, Some("lib/libvk.dylib")),
            NativeArtifact::SharedLibrary("lib/libvk.dylib".to_owned())
        );
        assert_eq!(
            NativeArtifact::from_paths(Some("lib/a.a"), Some("lib/a.dylib")),
            NativeArtifact::StaticArchive("lib/a.a".to_owned())
        );
    }

    #[test]
    fn a_pathless_dynamic_row_links_by_the_library_name() {
        // The corpus's `NativeTarget { triple: "...", dynamicLib: "" }`: the
        // library is found by its own name, so the row is not empty — it is
        // `-lvulkan`.
        let spec = NativeLibrarySpec::new(
            "vulkan",
            LinkMode::Dynamic,
            vec![NativeTargetSpec::new(
                triple("x86_64-linux-gnu"),
                NativeArtifact::from_paths(None, Some("")),
            )],
        )
        .expect("a pathless dynamic row is a valid declaration");
        let resolved = spec
            .resolve(Path::new("/pkg"), None, |_| panic!("no path to check"))
            .expect("resolution touches no path");
        assert_eq!(resolved.targets()[0].artifact(), None);
        assert_eq!(resolved.targets()[0].attributes().system_libs, ["vulkan"]);
    }

    #[test]
    fn a_dynamic_row_naming_frameworks_is_taken_at_its_word() {
        // `kira_metal` has no `libkira_metal` to find: its frameworks are the
        // symbols, so the library name must not be added to the link line.
        let spec = NativeLibrarySpec::new(
            "kira_metal",
            LinkMode::Dynamic,
            vec![
                NativeTargetSpec::new(triple("aarch64-macos-none"), NativeArtifact::None)
                    .with_attributes(NativeLinkAttributes {
                        frameworks: vec!["Metal".to_owned()],
                        ..NativeLinkAttributes::default()
                    }),
            ],
        )
        .expect("a valid declaration");
        let resolved = spec
            .resolve(Path::new("/pkg"), None, |_| true)
            .expect("resolution");
        assert!(resolved.targets()[0].attributes().system_libs.is_empty());
    }

    #[test]
    fn a_static_row_with_a_blank_path_is_still_refused() {
        // A blank path is no path: it must not join to the base directory.
        assert!(matches!(
            NativeLibrarySpec::new(
                "ffimath",
                LinkMode::Static,
                vec![NativeTargetSpec::static_archive(
                    triple("aarch64-macos-none"),
                    "",
                )],
            ),
            Err(NativeLibraryError::PathlessRow { .. })
        ));
    }

    #[test]
    fn rejects_a_nameless_library() {
        assert_eq!(
            NativeLibrarySpec::new("  ", LinkMode::Static, Vec::new())
                .expect_err("a nameless library is rejected"),
            NativeLibraryError::NamelessLibrary
        );
    }

    #[test]
    fn rejects_a_duplicate_target() {
        let error = NativeLibrarySpec::new(
            "ffimath",
            LinkMode::Static,
            vec![
                NativeTargetSpec::static_archive(triple("aarch64-macos-none"), "lib/a.a"),
                NativeTargetSpec::static_archive(triple("aarch64-macos-none"), "lib/b.a"),
            ],
        )
        .expect_err("a duplicate target is rejected");
        assert_eq!(
            error,
            NativeLibraryError::DuplicateTarget {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
            }
        );
    }

    #[test]
    fn resolve_joins_relative_to_the_base_dir() {
        let spec = NativeLibrarySpec::new(
            "ffimath",
            LinkMode::Static,
            vec![NativeTargetSpec::static_archive(
                triple("aarch64-macos-none"),
                "lib/libffimath-macos.a",
            )],
        )
        .expect("a valid declaration");
        let resolved = spec
            .resolve(Path::new("/pkg/NativeLibs"), None, |_| true)
            .expect("resolution with a satisfying predicate");
        assert_eq!(
            resolved.targets()[0].artifact(),
            Some(Path::new("/pkg/NativeLibs/lib/libffimath-macos.a"))
        );
    }

    #[test]
    fn resolve_reports_a_missing_archive() {
        let spec = NativeLibrarySpec::new(
            "ffimath",
            LinkMode::Static,
            vec![NativeTargetSpec::static_archive(
                triple("aarch64-macos-none"),
                "lib/absent.a",
            )],
        )
        .expect("a valid declaration");
        let error = spec
            .resolve(Path::new("/pkg/NativeLibs"), None, |_| false)
            .expect_err("a missing archive is rejected");
        assert_eq!(
            error,
            NativeLibraryError::MissingArchive(Box::new(MissingArchive {
                library: "ffimath".to_owned(),
                triple: triple("aarch64-macos-none"),
                path: PathBuf::from("/pkg/NativeLibs/lib/absent.a"),
            }))
        );
    }

    #[test]
    fn a_declared_link_mode_survives_the_round_trip() {
        assert_eq!(LinkMode::parse("Dynamic"), Some(LinkMode::Dynamic));
        assert_eq!(LinkMode::parse("static"), Some(LinkMode::Static));
        assert_eq!(LinkMode::parse("Weak"), None);
        assert_eq!(
            AutobindMode::parse("all_public"),
            Some(AutobindMode::AllPublic)
        );
    }
}
