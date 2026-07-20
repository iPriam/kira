//! Resolved dependency packages whose source modules may satisfy imports.
//!
//! A package root owns the import namespace named by its manifest dependency
//! entry. Unlike a bundled root, it is supplied by project resolution rather
//! than discovered from the installed toolchain.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// A resolved dependency package available to the module-loading walk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageRoot {
    /// The first import-path segment this package owns.
    pub name: String,
    /// The directory containing the package's Kira source files.
    pub source_dir: PathBuf,
}

impl PackageRoot {
    /// Creates a package named `name` whose modules live under `source_dir`.
    #[must_use]
    pub fn new(name: impl Into<String>, source_dir: impl Into<PathBuf>) -> Self {
        Self {
            name: name.into(),
            source_dir: source_dir.into(),
        }
    }

    /// Whether `module` falls inside this package's namespace.
    ///
    /// Ownership uses segment equality, so `Core` owns `Core.Text` but not
    /// `CoreHelpers`.
    #[must_use]
    pub fn owns(&self, module: &str) -> bool {
        module
            .split('.')
            .next()
            .is_some_and(|first| first == self.name)
    }

    /// Returns the module path relative to this package's import namespace.
    ///
    /// The package root itself keeps its name (`Core` maps to `Core.kira`), while
    /// a child drops the namespace prefix (`Core.Text` maps to `Text.kira`).
    #[must_use]
    pub fn relative_module<'a>(&self, module: &'a str) -> Option<&'a str> {
        if module == self.name {
            return Some(module);
        }
        module.strip_prefix(&self.name)?.strip_prefix('.')
    }

    /// Enumerates every `.kira` source file below this package's source directory.
    ///
    /// Files are returned in path order so package aggregation is deterministic.
    /// Unreadable subdirectories are skipped; the frontend reports imports that
    /// ultimately found no readable source.
    #[must_use]
    pub fn source_files(&self) -> Vec<PathBuf> {
        package_source_files(&self.source_dir)
    }
}

/// Enumerates every `.kira` source file below `source_dir` in path order.
#[must_use]
pub fn package_source_files(source_dir: &Path) -> Vec<PathBuf> {
    let mut directories = vec![source_dir.to_path_buf()];
    let mut files = Vec::new();

    while let Some(directory) = directories.pop() {
        let Ok(entries) = std::fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(path);
            } else if path.extension() == Some(OsStr::new("kira")) {
                files.push(path);
            }
        }
    }

    files.sort_unstable();
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ownership_compares_the_first_whole_segment() {
        let root = PackageRoot::new("Core", "/packages/core/app");
        assert!(root.owns("Core"));
        assert!(root.owns("Core.Text"));
        assert!(!root.owns("CoreHelpers"));
        assert!(!root.owns("Text"));
    }

    #[test]
    fn package_relative_modules_drop_only_the_owned_prefix() {
        let root = PackageRoot::new("Core", "/packages/core/app");
        assert_eq!(root.relative_module("Core"), Some("Core"));
        assert_eq!(root.relative_module("Core.Text"), Some("Text"));
        assert_eq!(
            root.relative_module("Core.Text.Category"),
            Some("Text.Category")
        );
        assert_eq!(root.relative_module("CoreHelpers"), None);
        assert_eq!(root.relative_module("Text"), None);
    }

    #[test]
    fn source_files_include_nested_kira_files_only() {
        let directory = std::env::temp_dir().join("kira-package-root-enumeration");
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(directory.join("Nested")).expect("create source directories");
        std::fs::write(directory.join("Text.kira"), "").expect("write root module");
        std::fs::write(directory.join("Nested/Category.kira"), "").expect("write nested module");
        std::fs::write(directory.join("ignored.txt"), "").expect("write non-Kira file");

        let files = package_source_files(&directory);
        let relative: Vec<PathBuf> = files
            .iter()
            .filter_map(|path| path.strip_prefix(&directory).ok().map(Path::to_path_buf))
            .collect();
        let _ = std::fs::remove_dir_all(&directory);

        assert_eq!(
            relative,
            vec![
                PathBuf::from("Nested/Category.kira"),
                PathBuf::from("Text.kira")
            ]
        );
    }
}
