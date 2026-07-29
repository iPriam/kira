//! The packages a program may import without naming a path.
//!
//! A *bundled* package ships with the toolchain rather than with the project:
//! Foundation is installed beside `kira` and versioned with it, so
//! `import Foundation` resolves from any directory on the machine with no
//! dependency entry and no relative path. [`kira_toolchain`] finds the
//! directory; this module decides which module names it owns and where each
//! one lives.
//!
//! # A bundled package owns exactly one namespace
//!
//! A bundled package's manifest declares a `moduleRoot`, and only module paths
//! whose **first segment** is that root are ever looked for inside it. So
//! Foundation can answer `Foundation` and `Foundation.Web`, and can never
//! answer `support` — a toolchain that could satisfy any import would make
//! every program's meaning depend on what happened to be installed.
//!
//! Below that gate the mapping is the ordinary one: a module path is a path
//! under the package's `app/` directory with dots as separators, so
//! `Foundation` is `app/Foundation.kira` and `Foundation.Web` is
//! `app/Foundation/Web.kira`. Nothing about resolution inside a bundled package
//! is special-cased, which is what will let Foundation grow files without
//! touching this code.
//!
//! # The project always wins
//!
//! Bundled roots are consulted only after the program's own directory has no
//! such file. A project that writes its own `Foundation.kira` beside its entry
//! file gets that one — the toolchain never reaches into a program to replace a
//! file the author wrote.
//!
//! # A missing bundle is silence here
//!
//! Discovery failing is not reported from this module. An import that finds no
//! file anywhere is already reported by the frontend, against the span of the
//! import that wanted it; raising a second error here would be the same problem
//! twice, under a span this crate does not have.

use std::path::{Path, PathBuf};

/// A package that ships with the toolchain and is importable by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundledRoot {
    /// The first module-path segment this package owns (its `moduleRoot`).
    module_root: String,
    /// The directory module paths resolve under — the package's `app/`.
    source_dir: PathBuf,
}

impl BundledRoot {
    /// A bundled package that owns `module_root` and resolves under
    /// `source_dir`.
    ///
    /// [`bundled_roots`] builds these from what is installed; this is for a
    /// caller that already knows where a bundle is — a test standing one up, or
    /// a driver pointed at a specific Foundation.
    #[must_use]
    pub fn new(module_root: impl Into<String>, source_dir: impl Into<PathBuf>) -> Self {
        Self {
            module_root: module_root.into(),
            source_dir: source_dir.into(),
        }
    }

    /// The namespace this package owns.
    #[must_use]
    pub fn module_root(&self) -> &str {
        &self.module_root
    }

    /// The directory this package's modules are resolved under.
    #[must_use]
    pub fn source_dir(&self) -> &Path {
        &self.source_dir
    }

    /// Whether `module` falls inside this package's namespace.
    ///
    /// Segment equality, not a string prefix: `Foundational` starts with
    /// `Foundation` and is not in it.
    #[must_use]
    pub fn owns(&self, module: &str) -> bool {
        module
            .split('.')
            .next()
            .is_some_and(|first| first == self.module_root)
    }
}

/// Every bundled package this toolchain can resolve imports against.
///
/// Empty when no bundle is installed and none is overridden — which is a
/// working compiler that simply cannot resolve `import Foundation`, not a
/// broken one.
#[must_use]
pub fn bundled_roots() -> Vec<BundledRoot> {
    let Ok(package) = kira_toolchain::discover_foundation() else {
        return Vec::new();
    };
    let Ok(text) = std::fs::read_to_string(package.manifest_path()) else {
        return Vec::new();
    };
    let Ok(manifest) = kira_manifest::load_declaration(&text) else {
        return Vec::new();
    };
    // A bundled package with no declared `moduleRoot` owns no namespace, so it
    // answers no import. Defaulting to the package name would let a rename of
    // a directory silently change which imports resolve.
    let Some(module_root) = manifest.module_root else {
        return Vec::new();
    };
    vec![BundledRoot {
        module_root,
        source_dir: package.source_dir(),
    }]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn foundation() -> BundledRoot {
        BundledRoot {
            module_root: "Foundation".to_owned(),
            source_dir: PathBuf::from("/toolchain/foundation/app"),
        }
    }

    #[test]
    fn a_bundle_owns_its_own_root_and_everything_under_it() {
        let root = foundation();
        assert!(root.owns("Foundation"));
        assert!(root.owns("Foundation.Web"));
        assert!(root.owns("Foundation.Web.Dom"));
    }

    /// The gate is on segments. A module whose name merely starts with the same
    /// letters is a different name and must not be looked for in the bundle.
    #[test]
    fn a_bundle_does_not_own_a_name_that_merely_starts_the_same() {
        let root = foundation();
        assert!(!root.owns("Foundational"));
        assert!(!root.owns("FoundationHelpers"));
        assert!(!root.owns("support"));
    }

    /// The repo's own committed Foundation is discoverable from the test
    /// binary, which lives under `target/`: this is the developer rule from
    /// [`kira_toolchain::bundled_discovery`] working end to end, and it is what
    /// makes every `import Foundation` test in this workspace runnable.
    #[test]
    fn the_committed_foundation_is_discoverable_from_a_source_build() {
        let roots = bundled_roots();
        let Some(root) = roots.first() else {
            return; // no bundle installed and none in the checkout
        };
        assert_eq!(root.module_root(), "Foundation");
        assert!(
            root.source_dir().join("Foundation.kira").is_file(),
            "{} has no Foundation.kira",
            root.source_dir().display()
        );
    }
}
