use super::*;

/// A scratch directory that removes itself, so a failing test leaves no
/// litter and no test depends on another's leftovers.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "kira-build-frontend-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        std::fs::create_dir_all(&base).expect("a scratch directory");
        TempDir(base)
    }

    fn write(&self, name: &str, text: &str) -> PathBuf {
        // Pushed component by component rather than joined whole: `join`
        // keeps an embedded `/` verbatim on Windows, so `app/Core.kira`
        // would produce a path spelled with a separator the rest of the
        // toolchain never emits, and comparing it against one `kira-project`
        // built would fail on Windows only.
        let mut path = self.0.clone();
        for component in name.split('/') {
            path.push(component);
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create fixture directories");
        }
        std::fs::write(&path, text).expect("write a fixture");
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_bare_file_compiles_as_an_application() {
    let dir = TempDir::new("bare");
    let path = dir.write("main.kira", "@Main function main() { print(1) return }");
    let compiled = compile(&path).expect("compile");
    assert_eq!(compiled.build_kind, BuildKind::Application);
    assert_eq!(compiled.package_name, None);
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    assert_eq!(compiled.ir.main, Some(0));
}

#[test]
fn a_library_package_reaches_the_frontend_by_its_manifest() {
    let dir = TempDir::new("lib");
    dir.write(
        "package.kira",
        "Package uifoundation {\n    let version = \"0.1.0\"\n    let kind = .Library\n}\n",
    );
    let path = dir.write("uifoundation.kira", "function f() { return }");
    let compiled = compile(&path).expect("compile");
    assert_eq!(compiled.build_kind, BuildKind::Library);
    assert_eq!(compiled.package_name.as_deref(), Some("uifoundation"));
    // No `@Main`, and no KSEM011: the manifest relaxed it.
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    assert_eq!(compiled.ir.main, None);
}

/// A manifest edited after the lockfile was written leaves the two
/// disagreeing. Compiling resolves the graph anyway, so it writes the
/// answer down instead of warning about it on every command from here on.
#[test]
fn compiling_rewrites_a_drifted_lockfile() {
    let dir = TempDir::new("lockfile-drift");
    dir.write(
        "package.kira",
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    let stale = "version = 1\n\n[root]\nname = \"Core\"\n\n[[package]]\nname = \"Ghost\"\n";
    dir.write("kira.lock", stale);
    let path = dir.write("app/Core.kira", "function value() -> Int { return 1 }");

    let compiled = compile(&path).expect("compile");

    let lock = std::fs::read_to_string(dir.0.join("kira.lock")).expect("read lockfile");
    assert_ne!(stale, lock, "the stale lockfile should have been rewritten");
    assert!(lock.contains("name = \"Core\""), "{lock}");
    assert!(!lock.contains("Ghost"), "{lock}");
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPK026")),
        "{:?}",
        compiled.diagnostics
    );
    assert!(
        !compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KPK024")),
        "the drift warning is replaced by the synced note: {:?}",
        compiled.diagnostics
    );
}

/// A project without a lockfile has not asked for one; compiling must not
/// create files the command was never pointed at.
#[test]
fn compiling_does_not_create_a_missing_lockfile() {
    let dir = TempDir::new("lockfile-absent");
    dir.write(
        "package.kira",
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    let path = dir.write("app/Core.kira", "function value() -> Int { return 1 }");

    compile(&path).expect("compile");

    assert!(!dir.0.join("kira.lock").exists());
}

#[test]
fn a_library_directory_compiles_every_source_under_app() {
    let dir = TempDir::new("aggregate-library");
    dir.write(
        "package.kira",
        "Package Core {\n    let kind = .Library\n    let moduleRoot = \"Core\"\n}\n",
    );
    let entry = dir.write("app/Core.kira", "function value() -> Int { return 1 }");
    let broken = dir.write("app/Broken.kira", "function broken(");

    let target = kira_project::resolve_target(&dir.0).expect("resolve library directory");
    assert_eq!(target.source_path.as_deref(), entry.to_str());
    let compiled = compile(Path::new(
        target
            .source_path
            .as_deref()
            .expect("library target compilation entry"),
    ))
    .expect("reach the frontend");

    assert!(compiled.has_errors(), "{:?}", compiled.diagnostics);
    assert!(
        compiled
            .sources
            .iter()
            .any(|source| source.path == broken.display().to_string()),
        "{:?}",
        compiled.sources
    );
    let rendered = compiled
        .diagnostics
        .iter()
        .map(|diagnostic| kira_diagnostics::renderer::render(diagnostic, &compiled.sources))
        .collect::<String>();
    assert!(
        rendered.contains(&broken.display().to_string()),
        "{rendered}"
    );
}

#[test]
fn a_relative_entry_path_resolves_its_boundary_manifest() {
    const CHILD_PROCESS: &str = "KIRA_BUILD_RELATIVE_ENTRY_CHILD";

    if std::env::var_os(CHILD_PROCESS).is_some() {
        let compiled =
            compile(Path::new("app/main.kira")).expect("compile the relative package entry");
        assert_eq!(compiled.build_kind, BuildKind::Application);
        assert_eq!(compiled.package_name.as_deref(), Some("RelativeApp"));
        assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
        return;
    }

    let dir = TempDir::new("relative-entry");
    dir.write(
        "package.kira",
        "Package RelativeApp {\n    let kind = .App\n}\n",
    );
    dir.write("app/main.kira", "@Main function main() { return }");

    let current_thread = std::thread::current();
    let test_name = current_thread.name().expect("the libtest test name");
    let output = std::process::Command::new(std::env::current_exe().expect("the test binary"))
        .arg("--exact")
        .arg(test_name)
        .arg("--nocapture")
        .env(CHILD_PROCESS, "1")
        .current_dir(&dir.0)
        .output()
        .expect("run the relative-path test in its package directory");
    assert!(
        output.status.success(),
        "child test failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
fn a_package_compiles_with_resolver_fed_dependency_modules() {
    let dir = TempDir::new("resolved-package");
    dir.write(
        "editor/package.kira",
        r#"Package EditorApp {
    let version = "0.1.0"
    let kind = .App
    let dependencies = [Dependency { name: "Core", path: "../core" }]
    let defaults = Defaults { executionMode: Backend.Llvm, buildTarget: BuildTarget.Host }
}
"#,
    );
    dir.write(
        "core/package.kira",
        r#"Package Core {
    let version = "0.1.0"
    let kind = .Library
    let moduleRoot = "Core"
}
"#,
    );
    let values = std::fs::canonicalize(dir.write(
        "core/app/Values.kira",
        "function coreValue() -> Int { return 41 }",
    ))
    .expect("canonical values path");
    let broken = std::fs::canonicalize(dir.write(
        "core/app/Broken.kira",
        "function brokenValue() -> Int { return missingFromCore }",
    ))
    .expect("canonical broken path");
    let entry = dir.write(
        "editor/app/main.kira",
        "import Core\n@Main function main() { print(coreValue() + 1) return }",
    );

    let compiled = compile(&entry).expect("compile a resolved package graph");
    let source_paths = compiled
        .sources
        .iter()
        .map(|source| source.path.as_str())
        .collect::<Vec<_>>();
    assert!(
        source_paths.contains(&values.to_string_lossy().as_ref()),
        "{source_paths:?}"
    );
    assert!(
        source_paths.contains(&broken.to_string_lossy().as_ref()),
        "{source_paths:?}"
    );
    assert!(
        !compiled
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.has_code("KSEM032")),
        "{:?}",
        compiled.diagnostics
    );
    assert!(
        !compiled.diagnostics.iter().any(|diagnostic| {
            diagnostic.has_code("KSEM060") && diagnostic.message.contains("coreValue")
        }),
        "{:?}",
        compiled.diagnostics
    );
    // An adopted autobind file intentionally reports KPK041 every time so
    // provenance is not forgotten. This graph test is about dependency
    // resolution, so keep refusing every other package-manager diagnostic
    // while allowing that informational adoption note.
    assert!(
        !compiled.diagnostics.iter().any(|diagnostic| {
            diagnostic
                .code
                .as_ref()
                .is_some_and(|code| code.as_str().starts_with("KPK") && code.as_str() != "KPK041")
        }),
        "{:?}",
        compiled.diagnostics
    );

    let library_diagnostic = compiled
        .diagnostics
        .iter()
        .find(|diagnostic| {
            diagnostic.has_code("KSEM060") && diagnostic.message.contains("missingFromCore")
        })
        .expect("the dependency module diagnostic");
    let rendered = kira_diagnostics::renderer::render(library_diagnostic, &compiled.sources);
    assert!(
        rendered.contains(&broken.display().to_string()),
        "{rendered}"
    );
    assert_eq!(compiled.default_execution_mode.as_deref(), Some("llvm"));
    assert_eq!(compiled.default_build_target.as_deref(), Some("host"));
}

#[test]
fn package_resolution_diagnostics_are_returned_with_frontend_diagnostics() {
    let dir = TempDir::new("resolution-diagnostic");
    dir.write(
        "app/package.kira",
        r#"Package BrokenApp {
    let dependencies = [Dependency { name: "Missing", path: "../missing" }]
}
"#,
    );
    let entry = dir.write(
        "app/app/main.kira",
        "import Missing\n@Main function main() { return }",
    );

    let compiled = compile(&entry).expect("resolution remains total below the root");
    let diagnostic = compiled
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.has_code("KPK020"))
        .expect("the missing dependency package diagnostic");
    assert!(diagnostic.primary_label().is_none());
    let rendered = kira_diagnostics::renderer::render(diagnostic, &compiled.sources);
    assert!(rendered.contains("error[KPK020]"), "{rendered}");
}

#[test]
fn a_missing_file_is_an_error_rather_than_an_empty_program() {
    let error = compile(Path::new("/nonexistent/kira-build/x.kira")).expect_err("a missing file");
    assert!(matches!(error, FrontendError::Read { .. }), "{error:?}");
}

#[test]
fn errors_come_back_as_diagnostics_rather_than_as_a_failure() {
    let dir = TempDir::new("bad");
    let path = dir.write(
        "main.kira",
        "@Main function main() { print(missing) return }",
    );
    let compiled = compile(&path).expect("compile");
    assert!(compiled.has_errors());
    assert!(compiled.diagnostics.iter().any(|d| d.has_code("KSEM060")));
}

/// The whole point of running autobind inside the frontend: a package that
/// declares `autobind` and ships no bindings compiles anyway, because the
/// bindings are written before a module is loaded. Without it, every call
/// into the C library is an undefined function and the caller gets blamed
/// for a file the build was supposed to write.
#[test]
fn a_declared_binding_is_generated_before_the_call_to_it_is_analyzed() {
    let dir = TempDir::new("autobind");
    dir.write(
            "package.kira",
            "Package demo {\n\
             \x20   let version = \"0.1.0\"\n\
             \x20   let kind = PackageKind.App\n\
             \x20   let nativeLibraries = [\n\
             \x20       NativeLibrary {\n\
             \x20           name: \"demo\",\n\
             \x20           linkMode: LinkMode.Static,\n\
             \x20           autobind: Autobind { module: \"demo\", headers: [\"NativeLibs/demo.h\"], mode: AutobindMode.AllPublic },\n\
             \x20           nativeTargets: [\n\
             \x20               NativeTarget { triple: \"HOST_TRIPLE\", staticLib: \"generated/libdemo.a\" }\n\
             \x20           ],\n\
             \x20       }\n\
             \x20   ]\n\
             }\n"
                .replace("HOST_TRIPLE", &kira_project::host_target().to_string())
                .as_str(),
        );
    dir.write(
        "NativeLibs/demo.h",
        "double demo_measure(const char *text, double size);\n",
    );
    let entry = dir.write(
        "app/main.kira",
        "@Main function main() { print(demo_measure(\"hi\", 14.0)) return }",
    );

    let compiled = compile(&entry).expect("compile");
    assert!(!compiled.has_errors(), "{:?}", compiled.diagnostics);
    let generated = std::fs::read_to_string(dir.0.join("app/bindings/demo.kira"))
        .expect("the binding was written into the package");
    assert!(generated.contains("symbol: demo_measure"), "{generated}");
}

#[test]
fn a_types_file_outside_bind_types_is_reported() {
    let dir = TempDir::new("misplaced-bind-types");
    dir.write(
        "package.kira",
        "Package Gfx {\n    let kind = .Library\n    let moduleRoot = \"Gfx\"\n}\n",
    );
    let entry = dir.write("app/Gfx.kira", "function value() -> Int { return 1 }");
    // A `*_types.kira` file in `types/` rather than `bind-types/` is refused.
    dir.write("app/types/gfx_types.kira", "type Handle = RawPtr\n");

    let compiled = compile(&entry).expect("compile");
    assert!(
        compiled.diagnostics.iter().any(|d| d.has_code("KPK025")),
        "{:?}",
        compiled.diagnostics
    );
}

#[test]
fn a_types_file_inside_bind_types_is_accepted() {
    let dir = TempDir::new("placed-bind-types");
    dir.write(
        "package.kira",
        "Package Gfx {\n    let kind = .Library\n    let moduleRoot = \"Gfx\"\n}\n",
    );
    let entry = dir.write("app/Gfx.kira", "function value() -> Int { return 1 }");
    dir.write("app/bind-types/gfx_types.kira", "type Handle = RawPtr\n");

    let compiled = compile(&entry).expect("compile");
    assert!(
        !compiled.diagnostics.iter().any(|d| d.has_code("KPK025")),
        "{:?}",
        compiled.diagnostics
    );
}
