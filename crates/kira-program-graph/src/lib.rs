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
//! turn a working program into a compile error. A cycle is the one shape with
//! no dependencies-first order to return; the walk still terminates and still
//! returns every module once, which is all a cyclic program can be given.
//!
//! # Bundled packages
//!
//! A module the program's own directory does not hold may still be found in a
//! package that ships with the toolchain — that is how `import Foundation`
//! resolves with no path and no dependency entry. See [`bundled`] for which
//! names a bundle owns and why the project's own files always win.

pub mod bundled;

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use bundled::BundledRoot;
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

/// One unit of the depth-first walk: a module still to be visited, or one whose
/// imports have all been visited and which is therefore ready to be recorded.
///
/// Making the emission an explicit step is what turns the walk from pre-order
/// into post-order without recursion — a module's `Emit` is pushed under the
/// `Visit`s of everything it imports, so it comes back off the stack after all
/// of them.
enum Step {
    /// Load this module and schedule its imports.
    Visit(String),
    /// Record this module; everything it imports is already recorded.
    Emit(Box<ModuleSource>),
}

/// Reads every module the program at `entry_path` imports, transitively.
///
/// Modules come back **dependencies first**: the walk is depth-first
/// *post-order*, so a module is recorded only after every module it imports has
/// been, and a declaration may name a type from a module it imports regardless
/// of the order the entry file happens to list its own imports in. Where the
/// graph has a topological order, this is one.
///
/// The order is the one the frontend assigns source ids in — see
/// [`kira_semantics::module_source_id`] — so a caller mirroring it into a
/// [`kira_source::SourceMap`] must insert the entry file first and then these,
/// in order.
///
/// An import naming a file that cannot be read is skipped; the frontend reports
/// it as an unresolved import, where the import's span is.
#[must_use]
pub fn load_modules(entry_path: &Path, entry_text: &str) -> Vec<ModuleSource> {
    load_modules_with(entry_path, entry_text, &bundled::bundled_roots())
}

/// [`load_modules`], against an explicit set of bundled packages.
///
/// Split out so a test can hand the walk a bundle it built itself rather than
/// whichever toolchain the machine happens to have installed. Callers compiling
/// a real program want [`load_modules`], which discovers the bundles.
#[must_use]
pub fn load_modules_with(
    entry_path: &Path,
    entry_text: &str,
    bundles: &[BundledRoot],
) -> Vec<ModuleSource> {
    // The module root is the entry file's directory. A module path is a
    // sequence of identifiers, so it can name nothing above the root: there is
    // no `..` an import could spell, which is what keeps a program's modules
    // inside the program without a separate containment check.
    let root = entry_path.parent().unwrap_or_else(|| Path::new("."));
    let mut loaded: Vec<ModuleSource> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut stack: Vec<Step> = Vec::new();
    push_visits(&mut stack, imports_of(entry_text));

    while let Some(step) = stack.pop() {
        let module = match step {
            Step::Emit(source) => {
                loaded.push(*source);
                continue;
            }
            Step::Visit(module) => module,
        };
        if !seen.insert(module.clone()) {
            continue;
        }
        if seen.len() > MAX_MODULES {
            break;
        }
        let Some((path, text)) = read_module(root, &module, bundles) else {
            // Absent, or unreadable. Either way the frontend says so, with the
            // span of the import that wanted it; reporting here as well would
            // be the same problem twice under two different spans.
            continue;
        };
        let nested = imports_of(&text);
        stack.push(Step::Emit(Box::new(ModuleSource {
            module,
            path: path.to_string_lossy().into_owned(),
            text,
        })));
        push_visits(&mut stack, nested);
    }

    loaded
}

/// Schedules `modules` to be visited, first one first.
///
/// The stack pops in reverse, so the list goes on reversed: source order is
/// what decides which of two independent modules is loaded first, and a
/// program's module list should not depend on a stack's direction.
fn push_visits(stack: &mut Vec<Step>, modules: Vec<String>) {
    stack.extend(modules.into_iter().rev().map(Step::Visit));
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

/// Reads `module`, from the program's own directory first and from a bundled
/// package only if the program does not hold it.
///
/// The order is the rule: a file the author wrote beside their program always
/// wins over one that came with the toolchain, so installing a new Foundation
/// can never change the meaning of a program that shipped its own module by
/// that name. The bundle is a fallback, never an override.
///
/// Returns the path it read from together with the text, because the path is
/// what a diagnostic renders against — a span in Foundation must point at
/// Foundation's file, not at something under the user's directory.
fn read_module(root: &Path, module: &str, bundles: &[BundledRoot]) -> Option<(PathBuf, String)> {
    let own = module_path(root, module);
    if let Ok(text) = std::fs::read_to_string(&own) {
        return Some((own, text));
    }
    for bundle in bundles {
        if !bundle.owns(module) {
            continue;
        }
        let path = module_path(bundle.source_dir(), module);
        if let Ok(text) = std::fs::read_to_string(&path) {
            return Some((path, text));
        }
    }
    None
}

/// Where a dotted module path lives on disk, relative to a search root.
///
/// `support` is `support.kira`; `Foundation.Web` is `Foundation/Web.kira`. A
/// dot is a directory separator, which is what makes the module path a *name*
/// rather than a path — the source never spells a slash, an extension, or a
/// parent directory. The same mapping is used inside a bundled package, with
/// that package's `app/` as the root; nothing about a bundle's layout is
/// special-cased.
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

    /// Writes a throwaway module tree and returns the entry path.
    fn write_modules(name: &str, modules: &[(&str, &str)]) -> PathBuf {
        let directory = std::env::temp_dir().join(format!("kira-graph-{name}"));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).expect("create program directory");
        for (module, text) in modules {
            std::fs::write(directory.join(format!("{module}.kira")), text).expect("write module");
        }
        directory.join("main.kira")
    }

    /// The order is the graph's, not the entry file's: `main` lists `a` before
    /// `b`, and `a` must still come back first because `b` imports it.
    ///
    /// This is the order a pre-order walk gets wrong. It pops `a` and records
    /// it, then pops `b` and records it, giving `[a, b]` — which the final
    /// reverse then turns into `[b, a]`, putting `b` ahead of the module it
    /// depends on. Listing `b` first happens to come out right under both
    /// walks, so only this direction is a regression test.
    #[test]
    fn a_diamond_comes_back_dependencies_first() {
        let entry = write_modules(
            "diamond",
            &[
                ("a", "function aValue() -> Int { return 1 }"),
                ("b", "import a\nfunction bValue() -> Int { return 2 }"),
            ],
        );
        let loaded = load_modules(&entry, "import a\nimport b\n");
        let order: Vec<&str> = loaded.iter().map(|m| m.module.as_str()).collect();
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert_eq!(order, vec!["a", "b"]);
    }

    /// Two modules that import each other still terminate and still appear
    /// once each. A cycle is the one graph with no dependencies-first order, so
    /// this pins termination and completeness, not a particular sequence.
    #[test]
    fn a_cycle_terminates_with_each_module_once() {
        let entry = write_modules(
            "cycle",
            &[
                ("alpha", "import beta\nfunction a() -> Int { return 1 }"),
                ("beta", "import alpha\nfunction b() -> Int { return 2 }"),
            ],
        );
        let loaded = load_modules(&entry, "import alpha\n");
        let mut order: Vec<&str> = loaded.iter().map(|m| m.module.as_str()).collect();
        order.sort_unstable();
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert_eq!(order, vec!["alpha", "beta"]);
    }

    /// Builds a bundled package on disk and returns the root that names it.
    fn write_bundle(name: &str, module_root: &str, modules: &[(&str, &str)]) -> BundledRoot {
        let app = std::env::temp_dir()
            .join(format!("kira-bundle-{name}"))
            .join("app");
        let _ = std::fs::remove_dir_all(app.parent().expect("bundle root"));
        std::fs::create_dir_all(&app).expect("create bundle app directory");
        for (module, text) in modules {
            let path = module_path(&app, module);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("module directory");
            }
            std::fs::write(path, text).expect("write bundled module");
        }
        BundledRoot::new(module_root, app)
    }

    /// The mechanism: a module the program's directory does not hold is read
    /// out of a package that ships with the toolchain.
    #[test]
    fn a_bundled_module_resolves_without_a_path() {
        let bundle = write_bundle(
            "resolves",
            "Foundation",
            &[(
                "Foundation",
                "function printLine(text: borrow String) { print(text) return }",
            )],
        );
        let entry = write_modules("bundled-resolves", &[]);
        let loaded = load_modules_with(&entry, "import Foundation\n", &[bundle]);
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert_eq!(loaded.len(), 1, "{loaded:?}");
        assert_eq!(loaded[0].module, "Foundation");
        assert!(loaded[0].text.contains("printLine"), "{:?}", loaded[0].text);
        // The path is the bundle's, so a diagnostic in Foundation renders
        // against Foundation's own file.
        assert!(
            loaded[0].path.contains("kira-bundle-resolves"),
            "{}",
            loaded[0].path
        );
    }

    /// A dotted name inside a bundle is a directory under the bundle's `app/`,
    /// the same mapping the program's own directory uses.
    #[test]
    fn a_dotted_bundled_module_is_a_directory_under_the_bundle() {
        let bundle = write_bundle(
            "dotted",
            "Foundation",
            &[(
                "Foundation/Web",
                "function createElement() -> Int { return 1 }",
            )],
        );
        let entry = write_modules("bundled-dotted", &[]);
        let loaded = load_modules_with(&entry, "import Foundation.Web as Web\n", &[bundle]);
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert_eq!(loaded.len(), 1, "{loaded:?}");
        assert_eq!(loaded[0].module, "Foundation.Web");
    }

    /// The project always wins: a file the author wrote beside their program
    /// is the one that is loaded, even when the bundle has that name too.
    #[test]
    fn the_programs_own_file_beats_the_bundle() {
        let bundle = write_bundle(
            "shadowed",
            "Foundation",
            &[("Foundation", "function which() -> Int { return 1 }")],
        );
        let entry = write_modules(
            "bundled-shadowed",
            &[("Foundation", "function which() -> Int { return 2 }")],
        );
        let loaded = load_modules_with(&entry, "import Foundation\n", &[bundle]);
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert_eq!(loaded.len(), 1, "{loaded:?}");
        assert!(loaded[0].text.contains("return 2"), "{:?}", loaded[0].text);
    }

    /// A bundle answers only the namespace its manifest declares. A toolchain
    /// that could satisfy any import would make a program's meaning depend on
    /// what happened to be installed.
    #[test]
    fn a_bundle_does_not_answer_a_module_outside_its_root() {
        let bundle = write_bundle(
            "outside",
            "Foundation",
            &[("support", "function sneak() -> Int { return 1 }")],
        );
        let entry = write_modules("bundled-outside", &[]);
        let loaded = load_modules_with(&entry, "import support\n", &[bundle]);
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert!(loaded.is_empty(), "{loaded:?}");
    }

    /// With no bundle installed, `import Foundation` finds nothing and the walk
    /// returns nothing — the frontend reports it, against the import's span.
    #[test]
    fn no_bundle_leaves_the_import_unresolved_rather_than_failing() {
        let entry = write_modules("bundled-absent", &[]);
        let loaded = load_modules_with(&entry, "import Foundation\n", &[]);
        let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
        assert!(loaded.is_empty(), "{loaded:?}");
    }

    /// A word that merely looks like an import is not one: the reader is the
    /// real parser, so only a real `import` item counts.
    #[test]
    fn a_string_containing_the_word_import_is_not_an_import() {
        let names = imports_of("@Main function main() { print(\"import support\") return }");
        assert!(names.is_empty(), "{names:?}");
    }
}
