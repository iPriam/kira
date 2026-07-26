use super::*;

fn package_identity(package: &str, module: &str) -> String {
    kira_semantics::ImportTable::package_module_identity(package, module)
}

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

/// Builds a resolved dependency package and returns its import root.
fn write_package(name: &str, package_name: &str, modules: &[(&str, &str)]) -> PackageRoot {
    let app = std::env::temp_dir()
        .join(format!("kira-package-{name}"))
        .join("app");
    let _ = std::fs::remove_dir_all(app.parent().expect("package root"));
    std::fs::create_dir_all(&app).expect("create package app directory");
    for (module, text) in modules {
        let path = module_path(&app, module);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("module directory");
        }
        std::fs::write(path, text).expect("write package module");
    }
    PackageRoot::new(package_name, app)
}

fn remove_package(package: &PackageRoot) {
    if let Some(root) = package.source_dir.parent() {
        let _ = std::fs::remove_dir_all(root);
    }
}

/// The mechanism: a module the program's directory does not hold is read
/// out of a package that ships with the toolchain.
///
/// The bare name loads it *as a package*, so the root file arrives under the
/// package-scoped identity a dependency's root file gets — which is what lets a
/// bundle hold more than one file (see the sibling case below).
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
    assert_eq!(loaded[0].module, "Foundation::Foundation");
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

#[test]
fn an_entryless_package_import_exposes_its_namespace_and_all_source_files() {
    let package = write_package(
        "aggregate",
        "Core",
        &[
            ("Broken", "function brokenValue() -> Int { return 1 }"),
            ("Values", "function value() -> Int { return 2 }"),
        ],
    );
    let entry = write_modules("package-aggregate", &[]);

    let loaded =
        load_modules_with_packages(&entry, "import Core\n", &[], std::slice::from_ref(&package));
    let modules: Vec<&str> = loaded.iter().map(|source| source.module.as_str()).collect();
    let mut files: Vec<String> = loaded
        .iter()
        .filter(|source| Path::new(&source.path).extension() == Some(std::ffi::OsStr::new("kira")))
        .filter_map(|source| {
            Path::new(&source.path)
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .collect();
    files.sort_unstable();

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    assert_eq!(
        modules,
        vec![
            package_identity("Core", "Broken"),
            package_identity("Core", "Values"),
            package_identity("Core", "Core"),
        ]
    );
    assert_eq!(files, vec!["Broken.kira", "Values.kira"]);
}

/// A bare package-name import pulls in the package's whole surface: every
/// `.kira` file below its source directory, the `<name>.kira` root among them
/// and the siblings the root never imports (`Unused`). A package's files are
/// one flat scope, so a file is loaded because it is in the package, not because
/// something imported it — which is what let `Unused` go unread before.
#[test]
fn a_package_import_with_a_root_file_loads_every_sibling() {
    let package = write_package(
        "entry",
        "Core",
        &[
            (
                "Core",
                "import Helper\nfunction coreValue() -> Int { return helperValue() }",
            ),
            ("Helper", "function helperValue() -> Int { return 1 }"),
            ("Unused", "function unusedValue() -> Int { return 2 }"),
        ],
    );
    let entry = write_modules("package-entry", &[]);

    let loaded =
        load_modules_with_packages(&entry, "import Core\n", &[], std::slice::from_ref(&package));
    let mut identities: Vec<&str> = loaded.iter().map(|source| source.module.as_str()).collect();
    identities.sort_unstable();

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    // The root file anchors the bare `import Core` binding, so no empty alias is
    // added — every entry is a real file, and all three are present.
    assert_eq!(
        identities,
        vec![
            package_identity("Core", "Core"),
            package_identity("Core", "Helper"),
            package_identity("Core", "Unused"),
        ]
    );
}

/// A dotted sub-module import still names one file, not the whole package: it is
/// a specific module (`Core.Values`), so aggregation is exactly the bare-name
/// case and this one stays single-file.
#[test]
fn a_dotted_submodule_import_loads_only_that_file() {
    let package = write_package(
        "dotted-submodule",
        "Core",
        &[
            ("Core", "function coreValue() -> Int { return 0 }"),
            ("Values", "function value() -> Int { return 2 }"),
            ("Other", "function other() -> Int { return 3 }"),
        ],
    );
    let entry = write_modules("package-dotted-submodule", &[]);

    let loaded = load_modules_with_packages(
        &entry,
        "import Core.Values as Values\n",
        &[],
        std::slice::from_ref(&package),
    );
    let identities: Vec<&str> = loaded.iter().map(|source| source.module.as_str()).collect();

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    assert_eq!(identities, vec![package_identity("Core", "Values")]);
}

/// A cyclic import graph inside a package still loads: the walk is
/// visited-set-guarded, so two files that import each other terminate and each
/// appears once. Rejecting a cycle would turn a working flat package into a
/// compile error.
#[test]
fn a_cyclic_package_graph_still_loads() {
    let package = write_package(
        "cyclic",
        "Core",
        &[
            (
                "Core",
                "import Ring\nfunction coreValue() -> Int { return ringValue() }",
            ),
            (
                "Ring",
                "import Core\nfunction ringValue() -> Int { return coreValue() }",
            ),
        ],
    );
    let entry = write_modules("package-cyclic", &[]);

    let loaded =
        load_modules_with_packages(&entry, "import Core\n", &[], std::slice::from_ref(&package));
    let mut identities: Vec<&str> = loaded.iter().map(|source| source.module.as_str()).collect();
    identities.sort_unstable();

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    assert_eq!(
        identities,
        vec![
            package_identity("Core", "Core"),
            package_identity("Core", "Ring"),
        ]
    );
}

#[test]
fn a_project_import_still_prefers_the_projects_own_file() {
    let package = write_package(
        "project-precedence",
        "Core",
        &[("Core", "function which() -> Int { return 1 }")],
    );
    let entry = write_modules(
        "package-project-precedence",
        &[("Core", "function which() -> Int { return 2 }")],
    );

    let loaded =
        load_modules_with_packages(&entry, "import Core\n", &[], std::slice::from_ref(&package));

    assert_eq!(loaded.len(), 1, "{loaded:?}");
    assert_eq!(loaded[0].module, "Core");
    assert!(loaded[0].text.contains("return 2"), "{}", loaded[0].text);

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
}

#[test]
fn a_dependency_sibling_beats_the_consumers_same_named_file() {
    let package = write_package(
        "captured-sibling",
        "Core",
        &[
            (
                "Core",
                "import Helper\nfunction coreValue() -> Int { return helperValue() }",
            ),
            ("Helper", "function helperValue() -> Int { return 1 }"),
        ],
    );
    let entry = write_modules(
        "package-captured-sibling",
        &[("Helper", "function helperValue() -> Int { return 99 }")],
    );

    let loaded =
        load_modules_with_packages(&entry, "import Core\n", &[], std::slice::from_ref(&package));
    let helper = loaded
        .iter()
        .find(|source| source.module == package_identity("Core", "Helper"))
        .expect("the dependency helper is loaded");

    assert!(helper.text.contains("return 1"), "{}", helper.text);
    assert!(
        helper
            .path
            .starts_with(package.source_dir.to_string_lossy().as_ref()),
        "{}",
        helper.path
    );
    assert!(
        loaded
            .iter()
            .all(|source| !source.text.contains("return 99")),
        "{loaded:?}"
    );

    remove_package(&package);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
}

#[test]
fn same_named_files_from_two_packages_do_not_collide() {
    let first = write_package(
        "same-name-first",
        "First",
        &[("Services", "function firstService() -> Int { return 1 }")],
    );
    let second = write_package(
        "same-name-second",
        "Second",
        &[("Services", "function secondService() -> Int { return 2 }")],
    );
    let entry = write_modules("package-same-name", &[]);

    let loaded = load_modules_with_packages(
        &entry,
        "import First\nimport Second\n",
        &[],
        &[first.clone(), second.clone()],
    );
    let identities: Vec<&str> = loaded.iter().map(|source| source.module.as_str()).collect();

    remove_package(&first);
    remove_package(&second);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    assert!(
        identities.contains(&package_identity("First", "Services").as_str()),
        "{loaded:?}"
    );
    assert!(
        identities.contains(&package_identity("Second", "Services").as_str()),
        "{loaded:?}"
    );
}

/// A word that merely looks like an import is not one: the reader is the
/// real parser, so only a real `import` item counts.
#[test]
fn a_string_containing_the_word_import_is_not_an_import() {
    let names = imports_of("@Main function main() { print(\"import support\") return }");
    assert!(names.is_empty(), "{names:?}");
}

/// A bundle is a package, so its bare-name import pulls in every file below its
/// source directory — the rule a dependency import already followed.
///
/// Foundation grew a second file the moment it grew a filesystem. Reading only
/// `Foundation.kira` would have left that file unreachable by any spelling: it
/// is not a submodule anyone writes an import for, it is more of Foundation.
#[test]
fn a_bare_bundle_import_loads_every_file_in_the_bundle() {
    let bundle = write_bundle(
        "aggregate",
        "Foundation",
        &[
            (
                "Foundation",
                "function printLine(text: borrow String) { print(text) return }",
            ),
            (
                "FileSystem",
                "function fileExists(path: borrow String) -> Bool { return fsFileExists(path) }",
            ),
        ],
    );
    let entry = write_modules("bundled-aggregate", &[]);
    let loaded = load_modules_with(&entry, "import Foundation\n", &[bundle]);
    let _ = std::fs::remove_dir_all(entry.parent().expect("program directory"));
    assert_eq!(loaded.len(), 2, "{loaded:?}");
    assert!(
        loaded.iter().any(|read| read.text.contains("printLine")),
        "{loaded:?}"
    );
    assert!(
        loaded.iter().any(|read| read.text.contains("fileExists")),
        "{loaded:?}"
    );
}
