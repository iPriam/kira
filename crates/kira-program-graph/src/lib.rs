//! Whole-program module graph construction: reading the files an entry program
//! imports.
//!
//! Layer 6 of the Kira package graph.
//!
//! Imports are file-scoped, but *loading* them is not a per-file question: a
//! module imported by a module is still part of the program, so this walks the
//! import graph transitively from the entry file and returns every module it
//! reached.
//!
//! # Why loading lives here and resolution does not
//!
//! `kira-semantics` decides which import binds which name, and reports the ones
//! that bind nothing. It cannot decide which *file* an import names, because it
//! has no filesystem — it compiles for `wasm32-unknown-unknown`. So this crate
//! does the one thing that needs a disk: turn `import support` into the text of
//! `support.kira`, and hand the texts to the frontend as an input.
//!
//! # Cycles
//!
//! A module already loaded is never loaded again, so two modules that import
//! each other terminate and appear in the program once each. That is not a
//! leniency to be tightened later: the reference implementation accepts a
//! cyclic import graph for exactly this reason, and rejecting one here would
//! turn a working program into a compile error.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kira_semantics::ModuleSource;
use kira_source::SourceId;
use kira_syntax_model::ast::Item;

/// The maximum number of modules one program may be built from.
///
/// A bound rather than a promise of unboundedness: the module ids are handed to
/// [`kira_source::SourceMap`] and to salsa as small integers, and a program
/// that reached this many files has a generator loop, not a design. Hitting it
/// stops the walk instead of growing without limit.
const MAX_MODULES: usize = 1024;

/// Reads every module the program at `entry_path` imports, transitively.
///
/// Modules come back **dependencies first**: a module's own imports are loaded
/// before it is, so a declaration may name a type from a module it imports.
/// The order is the one the frontend assigns source ids in — see
/// [`kira_semantics::module_source_id`] — so a caller mirroring it into a
/// [`kira_source::SourceMap`] must insert the entry file first and then these,
/// in order.
///
/// An import naming a file that cannot be read is skipped; the frontend reports
/// it as an unresolved import, where the import's span is.
#[must_use]
pub fn load_modules(entry_path: &Path, entry_text: &str) -> Vec<ModuleSource> {
    // The module root is the entry file's directory. A module path is a
    // sequence of identifiers, so it can name nothing above the root: there is
    // no `..` an import could spell, which is what keeps a program's modules
    // inside the program without a separate containment check.
    let root = entry_path.parent().unwrap_or_else(|| Path::new("."));
    let mut loaded: Vec<ModuleSource> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut queue: Vec<String> = imports_of(entry_text);
    queue.reverse();

    while let Some(module) = queue.pop() {
        if !seen.insert(module.clone()) {
            continue;
        }
        if loaded.len() >= MAX_MODULES {
            break;
        }
        let path = module_path(root, &module);
        let Ok(text) = std::fs::read_to_string(&path) else {
            // Absent, or unreadable. Either way the frontend says so, with the
            // span of the import that wanted it; reporting here as well would
            // be the same problem twice under two different spans.
            continue;
        };
        let mut nested = imports_of(&text);
        nested.reverse();
        queue.extend(nested);
        loaded.push(ModuleSource {
            module,
            path: path.to_string_lossy().into_owned(),
            text,
        });
    }

    // The walk records a module *before* the ones it imports, so the list comes
    // out dependent-first; the frontend wants dependencies first.
    loaded.reverse();
    loaded
}

/// The dotted module paths a source file imports, in source order.
///
/// Parsing the file to read its imports is deliberate: the import grammar is
/// the parser's, and a second scanner that "just looked for `import`" would
/// disagree with it on the first file that wrote the word inside a string.
/// Diagnostics are discarded — this pass answers a filesystem question, and the
/// frontend parses the same text again and reports everything it finds.
fn imports_of(text: &str) -> Vec<String> {
    let parsed = kira_parser::parse(SourceId::new(0), text);
    parsed
        .tree
        .items()
        .iter()
        .filter_map(|item| match item {
            Item::Import(declaration) => {
                let segments: Vec<&str> = declaration
                    .path
                    .iter()
                    .map(|&segment| parsed.interner.resolve(segment))
                    .collect();
                match segments.is_empty() {
                    true => None,
                    false => Some(segments.join(".")),
                }
            }
            _ => None,
        })
        .collect()
}

/// Where a dotted module path lives on disk, relative to the module root.
///
/// `support` is `support.kira`; `Foundation.Web` is `Foundation/Web.kira`. A
/// dot is a directory separator, which is what makes the module path a *name*
/// rather than a path — the source never spells a slash, an extension, or a
/// parent directory.
fn module_path(root: &Path, module: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for segment in module.split('.') {
        path.push(segment);
    }
    path.set_extension("kira");
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_dotted_module_is_a_directory_path() {
        let path = module_path(Path::new("/app"), "Foundation.Web");
        assert_eq!(path, PathBuf::from("/app/Foundation/Web.kira"));
    }

    #[test]
    fn a_single_segment_module_is_a_sibling_file() {
        let path = module_path(Path::new("/app"), "support");
        assert_eq!(path, PathBuf::from("/app/support.kira"));
    }

    #[test]
    fn imports_are_read_in_source_order() {
        let names = imports_of(
            "import support\nimport Foundation.Web as Web\n@Main function main() { return }",
        );
        assert_eq!(names, vec!["support", "Foundation.Web"]);
    }

    /// A word that merely looks like an import is not one: the reader is the
    /// real parser, so only a real `import` item counts.
    #[test]
    fn a_string_containing_the_word_import_is_not_an_import() {
        let names = imports_of("@Main function main() { print(\"import support\") return }");
        assert!(names.is_empty(), "{names:?}");
    }
}
