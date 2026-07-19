//! What the generator emits, asserted on directly.
//!
//! The generated crate is compiled for real by `kira-export-consumer`, which is
//! the proof that it works. These tests are the complement: they pin the
//! decisions a compiler cannot check — that a stale-build guard is present at
//! all, that a keyword name is escaped rather than renamed, that an unused
//! import is never emitted — each of which would otherwise fail silently or in
//! somebody else's crate.

use std::path::Path;

use kira_bytecode::{ExportTable, ExportType, ModuleExport};

use crate::wrapper::*;

/// The motivating library's surface, in the shapes v1 supports.
pub(super) fn uifoundation() -> ExportTable {
    ExportTable {
        classes: vec!["Button".to_owned()],
        functions: vec![
            ModuleExport {
                name: "make_button".to_owned(),
                kira_name: "makeButton".to_owned(),
                function: 0,
                params: vec![ExportType::String],
                result: ExportType::Handle { class: 0 },
            },
            ModuleExport {
                name: "button_width".to_owned(),
                kira_name: "buttonWidth".to_owned(),
                function: 1,
                params: vec![ExportType::Handle { class: 0 }],
                result: ExportType::Int,
            },
            ModuleExport {
                name: "button_label".to_owned(),
                kira_name: "buttonLabel".to_owned(),
                function: 2,
                params: vec![ExportType::Handle { class: 0 }],
                result: ExportType::String,
            },
            ModuleExport {
                name: "click_at".to_owned(),
                kira_name: "clickAt".to_owned(),
                function: 3,
                params: vec![
                    ExportType::Handle { class: 0 },
                    ExportType::Int,
                    ExportType::Int,
                ],
                result: ExportType::Bool,
            },
        ],
    }
}

fn generated(exports: &ExportTable) -> GeneratedCrate {
    generate(&WrapperSpec {
        library: "uifoundation",
        version: "0.1.0",
        exports,
        content_hash: 0x0123_4567_89ab_cdef,
        toolchain_root: Path::new("/kira"),
    })
    .expect("generate")
}

pub(super) fn lib_rs(exports: &ExportTable) -> String {
    generated(exports)
        .file("src/lib.rs")
        .expect("lib.rs")
        .to_owned()
}

#[test]
fn the_crate_is_made_of_the_three_files_a_crate_needs() {
    let table = uifoundation();
    let generated = generated(&table);
    assert_eq!(generated.name, "uifoundation");
    let paths: Vec<String> = generated
        .files
        .iter()
        .map(|file| file.path.display().to_string())
        .collect();
    assert_eq!(paths, ["Cargo.toml", "README.md", "src/lib.rs"]);
}

#[test]
fn every_export_becomes_one_method_with_the_rust_types_of_its_signature() {
    let source = lib_rs(&uifoundation());
    assert!(
        source.contains("pub fn make_button(&self, arg0: &str) -> Result<Button<H>, Error> {"),
        "{source}"
    );
    assert!(
        source.contains("pub fn button_width(&self, arg0: &Button<H>) -> Result<i64, Error> {"),
        "{source}"
    );
    // A string result is owned by the caller, so it is `String` and not `&str`.
    assert!(
        source.contains("pub fn button_label(&self, arg0: &Button<H>) -> Result<String, Error> {"),
        "{source}"
    );
    assert!(
        source.contains(
            "pub fn click_at(&self, arg0: &Button<H>, arg1: i64, arg2: i64) -> Result<bool, Error> {"
        ),
        "{source}"
    );
}

#[test]
fn an_exported_class_becomes_a_newtype_that_releases_on_drop() {
    let source = lib_rs(&uifoundation());
    assert!(
        source.contains("pub struct Button<H: HostCapabilities = StdoutHost> {"),
        "{source}"
    );
    assert!(
        source.contains("impl<H: HostCapabilities> Drop for Button<H> {"),
        "{source}"
    );
    assert!(
        source.contains("let _ = self.instance.borrow_mut().release(self.handle);"),
        "{source}"
    );
}

#[test]
fn the_stale_build_guard_is_in_the_generated_source() {
    // The VM engine has no link step to fail, so the contract *is* the guard.
    // A generator that stopped emitting it would leave every wrapper silently
    // willing to call a library it was not generated from.
    let source = lib_rs(&uifoundation());
    assert!(
        source.contains("content_hash: 0x0123456789abcdef,"),
        "{source}"
    );
    assert!(source.contains("library.verify(&CONTRACT)?;"), "{source}");
    assert!(
        source.contains("classes: &[\n        \"Button\",\n    ],"),
        "{source}"
    );
}

#[test]
fn the_generated_crate_contains_no_unsafe() {
    // The VM engine's whole claim is that a consumer needs no unsafe and no
    // linker. Asserting it here keeps a future edit from quietly spending it.
    let source = lib_rs(&uifoundation());
    let code: String = source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!code.contains("unsafe"), "{code}");
}

#[test]
fn an_export_named_after_a_keyword_becomes_a_raw_identifier() {
    let table = ExportTable {
        classes: Vec::new(),
        functions: vec![ModuleExport {
            name: "match".to_owned(),
            kira_name: "match".to_owned(),
            function: 0,
            params: Vec::new(),
            result: ExportType::Int,
        }],
    };
    let source = lib_rs(&table);
    assert!(
        source.contains("pub fn r#match(&self) -> Result<i64, Error> {"),
        "{source}"
    );
    // The seam still calls it by the name the library published.
    assert!(source.contains("call(\"match\", &[])"), "{source}");
}

#[test]
fn a_library_with_no_exports_imports_nothing_it_does_not_use() {
    // An unused import here is a build failure in the consumer's crate,
    // reported against a file they did not write.
    let source = lib_rs(&ExportTable::default());
    assert!(!source.contains("NativeArg"), "{source}");
    assert!(!source.contains("NativeResult"), "{source}");
    assert!(!source.contains("ExpectedExport"), "{source}");
    assert!(!source.contains("ExportType"), "{source}");
    assert!(!source.contains("Handle"), "{source}");
    assert!(
        source.contains("pub struct Uifoundation<H: HostCapabilities = StdoutHost> {"),
        "{source}"
    );
}

#[test]
fn a_library_whose_exports_take_no_arguments_does_not_import_nativearg() {
    let table = ExportTable {
        classes: Vec::new(),
        functions: vec![ModuleExport {
            name: "tick".to_owned(),
            kira_name: "tick".to_owned(),
            function: 0,
            params: Vec::new(),
            result: ExportType::Void,
        }],
    };
    let source = lib_rs(&table);
    assert!(!source.contains("NativeArg"), "{source}");
    assert!(
        source.contains("use kira_runtime_abi::{HostCapabilities, NativeResult};"),
        "{source}"
    );
    assert!(source.contains("NativeResult::Void => Ok(()),"), "{source}");
}

#[test]
fn a_class_that_collides_with_the_library_type_is_refused_by_name() {
    let table = ExportTable {
        classes: vec!["Uifoundation".to_owned()],
        functions: Vec::new(),
    };
    let error = generate(&WrapperSpec {
        library: "uifoundation",
        version: "0.1.0",
        exports: &table,
        content_hash: 0,
        toolchain_root: Path::new("/kira"),
    })
    .expect_err("a collision");
    assert_eq!(
        error.to_string(),
        "`uifoundation` and `Uifoundation` both become the Rust name `Uifoundation`"
    );
}

#[test]
fn the_library_takes_a_custom_host_and_defaults_to_stdout() {
    // The VM is a portable core and the embedder supplies the effects: a
    // wrapper that only ever built a `StdoutHost` would decide for them where a
    // library's `print` goes, which is exactly what an embedder embedding it in
    // a log or a browser console cannot live with.
    let source = lib_rs(&uifoundation());
    assert!(
        source.contains("pub fn load() -> Result<Uifoundation<StdoutHost>, Error> {"),
        "{source}"
    );
    assert!(
        source.contains("pub fn load_with(host: H) -> Result<Uifoundation<H>, Error> {"),
        "{source}"
    );
    assert!(
        source.contains("library.instantiate_with(host)?"),
        "{source}"
    );
    // And the host is readable back, or a capturing host would be write-only.
    assert!(
        source.contains("pub fn with_host<R>(&self, read: impl FnOnce(&H) -> R) -> R {"),
        "{source}"
    );
    assert!(
        source.contains("pub fn with_host_mut<R>(&self, take: impl FnOnce(&mut H) -> R) -> R {"),
        "{source}"
    );
}

#[test]
fn a_class_named_after_the_host_parameter_is_refused_by_name() {
    // `impl<H> Drop for H<H>` does not compile. Saying so here beats saying it
    // in the consumer's build.
    let table = ExportTable {
        classes: vec!["H".to_owned()],
        functions: Vec::new(),
    };
    let error = generate(&WrapperSpec {
        library: "uifoundation",
        version: "0.1.0",
        exports: &table,
        content_hash: 0,
        toolchain_root: Path::new("/kira"),
    })
    .expect_err("a reserved name");
    assert_eq!(
        error.to_string(),
        "an exported class may not be named `H`: the generated wrapper spells its host \
         type parameter `H`"
    );
}

#[test]
fn a_handle_naming_no_class_is_refused_rather_than_rendered() {
    let table = ExportTable {
        classes: Vec::new(),
        functions: vec![ModuleExport {
            name: "f".to_owned(),
            kira_name: "f".to_owned(),
            function: 0,
            params: Vec::new(),
            result: ExportType::Handle { class: 3 },
        }],
    };
    let error = generate(&WrapperSpec {
        library: "uifoundation",
        version: "0.1.0",
        exports: &table,
        content_hash: 0,
        toolchain_root: Path::new("/kira"),
    })
    .expect_err("an unknown class");
    assert_eq!(error, WrapperError::UnknownClass { class: 3 });
}

#[test]
fn the_manifest_points_its_dependencies_at_the_toolchain_that_generated_it() {
    let table = uifoundation();
    let generated = generated(&table);
    let manifest = generated.file("Cargo.toml").expect("Cargo.toml");
    assert!(manifest.contains("name = \"uifoundation\""), "{manifest}");
    assert!(manifest.contains("version = \"0.1.0\""), "{manifest}");
    assert!(
        manifest.contains("kira-main = { path = \"/kira/crates/kira-main\" }"),
        "{manifest}"
    );
    assert!(manifest.contains("unsafe_code = \"forbid\""), "{manifest}");
}

#[test]
fn the_vm_manifest_says_it_has_no_build_script() {
    // Cargo auto-detects a build script by finding `build.rs` in the package
    // root, and the native engine writes one into this same directory. Saying
    // `build = false` makes "no build script" a fact of the manifest rather than
    // a fact about what a previous build happened to leave behind.
    let table = uifoundation();
    let generated = generated(&table);
    let manifest = generated.file("Cargo.toml").expect("Cargo.toml");
    assert!(manifest.contains("\nbuild = false\n"), "{manifest}");
}

#[test]
fn each_engine_knows_which_file_the_others_leave_behind() {
    // Three engines write into one directory, because a consumer's `path`
    // dependency names it and must survive a rebuild for any of them. Every
    // engine-specific file therefore has to appear in the list of every *other*
    // engine, or the switch flow keeps a stale one.
    assert_eq!(
        foreign_engine_files(Engine::Vm, "uifoundation"),
        [Path::new("build.rs"), Path::new("uifoundation.khm")]
    );
    assert_eq!(
        foreign_engine_files(Engine::Native, "uifoundation"),
        [Path::new("uifoundation.kbc"), Path::new("uifoundation.khm")]
    );
    assert_eq!(
        foreign_engine_files(Engine::Hybrid, "uifoundation"),
        [Path::new("build.rs")]
    );
}

#[test]
fn no_engine_deletes_a_file_it_writes_itself() {
    // The property the three lists above have to have, stated once rather than
    // read off them: an engine that listed its own artifact would delete what it
    // had just written, and the failure would look like a missing build rather
    // than a wrong list.
    let bytecode = PathBuf::from(artifact_file_name("uifoundation"));
    let manifest = PathBuf::from(manifest_file_name("uifoundation"));
    let script = PathBuf::from("build.rs");

    let vm = foreign_engine_files(Engine::Vm, "uifoundation");
    assert!(!vm.contains(&bytecode), "the VM engine embeds the bytecode");

    let native = foreign_engine_files(Engine::Native, "uifoundation");
    assert!(
        !native.contains(&script),
        "the native engine needs build.rs"
    );

    let hybrid = foreign_engine_files(Engine::Hybrid, "uifoundation");
    assert!(
        !hybrid.contains(&bytecode),
        "the hybrid engine embeds it too"
    );
    assert!(!hybrid.contains(&manifest), "and embeds its own manifest");
}

#[test]
fn the_readme_says_not_to_commit_it_and_lists_the_surface() {
    let table = uifoundation();
    let generated = generated(&table);
    let readme = generated.file("README.md").expect("README.md");
    assert!(readme.contains("do not commit"), "{readme}");
    assert!(
        readme.contains("| `makeButton` | `Uifoundation::make_button` |"),
        "{readme}"
    );
    assert!(readme.contains("`Button`"), "{readme}");
}

#[test]
fn the_artifact_is_embedded_from_the_crate_root_so_the_crate_relocates() {
    let source = lib_rs(&uifoundation());
    assert!(
        source.contains("include_bytes!(\"../uifoundation.kbc\")"),
        "{source}"
    );
    assert_eq!(artifact_file_name("uifoundation"), "uifoundation.kbc");
}

#[test]
fn generation_is_deterministic() {
    let table = uifoundation();
    assert_eq!(generated(&table), generated(&table));
}
