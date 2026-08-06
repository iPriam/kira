//! The filesystem answers assembling one program asks for over and over.
//!
//! Assembling a package is not one walk from one entry file. It is a walk from
//! the entry, then a walk from every other `.kira` file the package owns,
//! because a file no import reaches is still a member. Each of those walks
//! resolves the same imports — every file in a UI package writes `import
//! KiraGraphics` — and resolving `import <package>` means listing that
//! package's directory and selecting every file in it.
//!
//! Done directly, that is the same directory listed once per importing file,
//! the same hundred files read once per importing file, and the same hundred
//! files parsed once per importing file. On a program of a few hundred sources
//! the redundant work is two orders of magnitude larger than the real work.
//!
//! So the answers are memoized for the life of one assembly. Nothing here
//! decides anything — it reads, lists, canonicalizes, and scans imports exactly
//! as a direct call would — which is what makes the memo invisible: the walk
//! that uses it and a walk that does not produce the same modules in the same
//! order.
//!
//! # Why the life of one assembly and no longer
//!
//! The tree is read once per compilation and the compiler is a process. A cache
//! that outlived the assembly would need to know when a file changed, and the
//! consumer that rebuilds on a keystroke — the language server — assembles
//! again from the top for exactly that reason.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;

/// Memoized filesystem answers for one program assembly.
#[derive(Debug, Default)]
pub(crate) struct Sources {
    /// The `.kira` files below a package source directory, in path order.
    listings: HashMap<PathBuf, Rc<[PathBuf]>>,
    /// The canonical identity of a path, which is what stops a cycle.
    identities: HashMap<PathBuf, Option<PathBuf>>,
    /// The text of a file that was read successfully.
    ///
    /// Failures are not cached: a read that failed has an error to hand back,
    /// and the caller that wants one asks again.
    texts: HashMap<PathBuf, Rc<str>>,
    /// The module paths a file imports, parsed once.
    imports: HashMap<PathBuf, Rc<[String]>>,
}

impl Sources {
    /// Every `.kira` file below `source_dir`, in path order.
    pub(crate) fn listing(&mut self, source_dir: &Path) -> Rc<[PathBuf]> {
        if let Some(listed) = self.listings.get(source_dir) {
            return Rc::clone(listed);
        }
        let listed: Rc<[PathBuf]> = crate::package_roots::package_source_files(source_dir).into();
        self.listings
            .insert(source_dir.to_path_buf(), Rc::clone(&listed));
        listed
    }

    /// The identity two paths naming one file agree on.
    ///
    /// Canonical where the platform can say so, absolute where it cannot — the
    /// same fallback the walk has always used, so a path that canonicalizes on
    /// one machine and not another still stops its own cycle.
    pub(crate) fn identity(&mut self, path: &Path) -> Option<PathBuf> {
        if let Some(known) = self.identities.get(path) {
            return known.clone();
        }
        let identity = std::fs::canonicalize(path)
            .or_else(|_| std::path::absolute(path))
            .ok();
        self.identities.insert(path.to_path_buf(), identity.clone());
        identity
    }

    /// The text of `path`, reading it the first time it is asked for.
    pub(crate) fn read(&mut self, path: &Path) -> std::io::Result<Rc<str>> {
        if let Some(text) = self.texts.get(path) {
            return Ok(Rc::clone(text));
        }
        let text: Rc<str> = std::fs::read_to_string(path)?.into();
        self.texts.insert(path.to_path_buf(), Rc::clone(&text));
        Ok(text)
    }

    /// The text of `path`, or `None` when it cannot be read.
    pub(crate) fn text(&mut self, path: &Path) -> Option<Rc<str>> {
        self.read(path).ok()
    }

    /// The module paths `path` imports, parsing it the first time.
    pub(crate) fn imports(&mut self, path: &Path, text: &str) -> Rc<[String]> {
        if let Some(known) = self.imports.get(path) {
            return Rc::clone(known);
        }
        let parsed: Rc<[String]> = crate::imports_of(text).into();
        self.imports.insert(path.to_path_buf(), Rc::clone(&parsed));
        parsed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory this test owns and removes.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "kira-program-graph-sources-{}-{tag}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).expect("a scratch directory");
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_file_is_read_once_however_often_it_is_asked_for() {
        let scratch = Scratch::new("read-once");
        let path = scratch.0.join("a.kira");
        std::fs::write(&path, "import Foundation\n").expect("write the source");
        let mut sources = Sources::default();

        let first = sources.read(&path).expect("the first read");
        // The answer survives the file going away, which is what proves the
        // second call never touched the disk.
        std::fs::remove_file(&path).expect("remove the source");
        let second = sources.read(&path).expect("the memoized read");

        assert_eq!(&*first, "import Foundation\n");
        assert_eq!(first, second);
    }

    #[test]
    fn a_failed_read_is_not_remembered_as_an_answer() {
        let scratch = Scratch::new("read-later");
        let path = scratch.0.join("late.kira");
        let mut sources = Sources::default();

        assert!(sources.read(&path).is_err());
        std::fs::write(&path, "function f() { return }\n").expect("write the source");
        assert!(sources.read(&path).is_ok(), "a later read still works");
    }

    #[test]
    fn a_directory_is_listed_once() {
        let scratch = Scratch::new("listing");
        std::fs::write(scratch.0.join("a.kira"), "").expect("write a source");
        let mut sources = Sources::default();

        let first = sources.listing(&scratch.0);
        std::fs::write(scratch.0.join("b.kira"), "").expect("write another source");
        let second = sources.listing(&scratch.0);

        assert_eq!(first.len(), 1);
        assert_eq!(first, second, "the listing is the memoized one");
    }

    #[test]
    fn imports_are_parsed_once_per_file() {
        let scratch = Scratch::new("imports");
        let path = scratch.0.join("a.kira");
        let mut sources = Sources::default();

        let first = sources.imports(&path, "import Foundation\nimport Core.Text\n");
        // A different text for the same path comes back as the first answer:
        // one file has one import list within one assembly.
        let second = sources.imports(&path, "");

        assert_eq!(&*first, ["Foundation".to_owned(), "Core.Text".to_owned()]);
        assert_eq!(first, second);
    }
}
