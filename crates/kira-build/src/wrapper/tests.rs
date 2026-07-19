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

use super::*;

/// The motivating library's surface, in the shapes v1 supports.
fn uifoundation() -> ExportTable {
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

fn lib_rs(exports: &ExportTable) -> String {
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
fn each_engine_knows_which_file_the_other_one_leaves_behind() {
    // The two lists are disjoint and neither is empty: an engine that claimed to
    // leave nothing behind would let the switch flow keep a stale file.
    assert_eq!(
        foreign_engine_files(Engine::Vm, "uifoundation"),
        [Path::new("build.rs")]
    );
    assert_eq!(
        foreign_engine_files(Engine::Native, "uifoundation"),
        [Path::new("uifoundation.kbc")]
    );
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

/// The native-engine generator's tests.
///
/// The complement of the same idea one engine over: `kira-export-consumer`
/// compiles and *runs* this output against real machine code, so what is worth
/// asserting here is what a passing test could still have got wrong — a symbol
/// spelled two ways, a `build.rs` that links nothing, an ownership comment that
/// says the opposite of what the code does.
mod native {
    use kira_llvm_backend::NativeExportSurface;

    use super::*;
    use crate::wrapper::render_native::{NativeModel, build_rs, lib_rs as native_lib_rs};

    fn symbols() -> NativeExportSurface {
        crate::native::export_surface("uifoundation", &uifoundation())
    }

    fn model() -> NativeModel {
        NativeModel::build("uifoundation", &uifoundation(), &symbols()).expect("model")
    }

    fn generated() -> GeneratedCrate {
        generate_native(&NativeWrapperSpec {
            library: "uifoundation",
            version: "0.1.0",
            exports: &uifoundation(),
            symbols: &symbols(),
            toolchain_root: Path::new("/kira"),
            archive_directory: Path::new("/pkg/.kira-build/lib"),
        })
        .expect("generate")
    }

    #[test]
    fn every_declared_symbol_is_the_one_the_backend_was_told_to_emit() {
        // The whole guard rests on these two lists being one list. A wrapper
        // declaring `kira_lib_uifoundation_makeButton` against a backend that
        // emitted `..._make_button` is a link failure in the consumer's crate,
        // naming a symbol nobody wrote.
        let source = native_lib_rs(&model());
        let surface = symbols();
        for function in &surface.functions {
            assert!(
                source.contains(&format!("fn {}(args: *const BridgeValue", function.symbol)),
                "`{}` is not declared: {source}",
                function.symbol
            );
        }
        for class in &surface.classes {
            assert!(
                source.contains(&format!("fn {}(args: *const BridgeValue", class.symbol)),
                "`{}` is not declared: {source}",
                class.symbol
            );
        }
    }

    #[test]
    fn load_calls_the_marker_so_a_stale_archive_fails_the_link() {
        // The native engine's entire stale-build guard: `load()` references a
        // symbol only a library built under this contract defines. A `load()`
        // that did not call it would link against anything.
        let source = native_lib_rs(&model());
        assert!(
            source.contains("unsafe { kira_lib_uifoundation_abi_1() }"),
            "{source}"
        );
    }

    #[test]
    fn a_string_argument_is_lent_and_a_string_result_is_taken() {
        // The `marshal.rs` contract, in the two directions that differ: the
        // wrapper never frees a string it passed in (the callee does), and it
        // always frees one it got back (after copying the bytes out).
        let source = native_lib_rs(&model());
        assert!(source.contains("fn lend_str(text: &str)"), "{source}");
        assert!(source.contains("kira_rt_str_new(text.as_ptr()"), "{source}");
        // Nothing frees an argument on this side.
        let lend = source
            .split("fn lend_str")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("the lend_str body");
        assert!(!lend.contains("kira_rt_str_free"), "{lend}");
        // And a result is always freed, exactly once.
        let take = source
            .split("fn take_str")
            .nth(1)
            .and_then(|rest| rest.split("\nfn ").next())
            .expect("the take_str body");
        assert_eq!(take.matches("kira_rt_str_free").count(), 1, "{take}");
    }

    #[test]
    fn dropping_a_handle_calls_the_class_destructor_and_nothing_else_does() {
        let source = native_lib_rs(&model());
        let symbol = "kira_lib_uifoundation_drop_button";
        // Declared once in the `extern` block, called once from `Drop`, and
        // named once in its doc comment. Nothing else may reach it: a second
        // call site would be the double free the `Drop`-consumes-the-handle
        // design exists to make unrepresentable.
        assert_eq!(
            source.matches(&format!("unsafe {{ {symbol}(")).count(),
            1,
            "{source}"
        );
        assert!(
            source.contains(&format!("fn {symbol}(args: *const BridgeValue")),
            "{source}"
        );
        assert!(source.contains("impl Drop for Button {"), "{source}");
    }

    #[test]
    fn the_build_script_links_the_archive_by_the_name_a_linker_looks_for() {
        let script = build_rs(&model(), "/pkg/.kira-build/lib");
        assert!(
            script.contains("cargo:rustc-link-search=native=/pkg/.kira-build/lib"),
            "{script}"
        );
        assert!(
            script.contains("cargo:rustc-link-lib=static=uifoundation"),
            "{script}"
        );
    }

    #[test]
    fn a_surface_that_never_mentions_a_string_declares_no_string_helper() {
        // A declaration nobody calls is a `dead_code` warning in the consumer's
        // build, reported against a file they did not write — and a consumer
        // whose own gate is `-D warnings` gets a build failure. The VM engine's
        // renderer already holds this line; this is the native one's.
        let table = ExportTable {
            classes: Vec::new(),
            functions: vec![ModuleExport {
                name: "add".to_owned(),
                kira_name: "add".to_owned(),
                function: 0,
                params: vec![ExportType::Int, ExportType::Int],
                result: ExportType::Int,
            }],
        };
        let symbols = NativeExportSurface {
            abi_marker: Some("kira_lib_uifoundation_abi_1".to_owned()),
            functions: vec![kira_llvm_backend::NativeExport {
                symbol: "kira_lib_uifoundation_add".to_owned(),
                function: 0,
            }],
            classes: Vec::new(),
        };
        let model = NativeModel::build("uifoundation", &table, &symbols).expect("model");
        let source = native_lib_rs(&model);
        for unused in [
            "kira_rt_str_new",
            "kira_rt_str_free",
            "kira_rt_str_data",
            "kira_rt_str_len",
            "lend_str",
            "take_str",
            "NO_ARGS",
        ] {
            assert!(
                !source.contains(unused),
                "the wrapper declares `{unused}` and never calls it: {source}"
            );
        }
        // What it does need is still there.
        assert!(source.contains("fn result_slot()"), "{source}");
        assert!(source.contains("pub fn add("), "{source}");
    }

    #[test]
    fn an_export_taking_no_arguments_gets_the_empty_argument_array() {
        // The other side of the same rule: emitted when it is reached.
        let table = ExportTable {
            classes: Vec::new(),
            functions: vec![ModuleExport {
                name: "default_width".to_owned(),
                kira_name: "defaultWidth".to_owned(),
                function: 0,
                params: Vec::new(),
                result: ExportType::Int,
            }],
        };
        let symbols = NativeExportSurface {
            abi_marker: Some("kira_lib_uifoundation_abi_1".to_owned()),
            functions: vec![kira_llvm_backend::NativeExport {
                symbol: "kira_lib_uifoundation_default_width".to_owned(),
                function: 0,
            }],
            classes: Vec::new(),
        };
        let model = NativeModel::build("uifoundation", &table, &symbols).expect("model");
        let source = native_lib_rs(&model);
        assert!(
            source.contains("const NO_ARGS: [BridgeValue; 0] = [];"),
            "{source}"
        );
        assert!(source.contains("let args = NO_ARGS;"), "{source}");
    }

    #[test]
    fn the_build_script_names_exactly_the_platform_libraries_the_backend_owns() {
        // The list has one home (`kira_llvm_backend::PLATFORM_LINK_LISTS`) and
        // three readers: this compiler's linker, this generated script, and a
        // consumer reaching the wrapper another way. This is the check that the
        // generated one is rendered from the home rather than copied beside it —
        // a library added there and not here fails a consumer's link naming
        // nothing.
        let script = build_rs(&model(), "/pkg/.kira-build/lib");
        for list in kira_llvm_backend::PLATFORM_LINK_LISTS {
            for name in list.libraries.iter().chain(list.frameworks) {
                assert!(
                    script.contains(&format!("\"{name}\"")),
                    "the build script does not name `{name}`: {script}"
                );
            }
            if !list.libraries.is_empty() || !list.frameworks.is_empty() {
                assert!(
                    script.contains(&format!("cfg!(target_os = \"{}\")", list.target_os)),
                    "the build script has no branch for {}: {script}",
                    list.target_os
                );
            }
        }
    }

    #[test]
    fn the_manifest_has_a_build_script_and_cannot_forbid_unsafe() {
        // Both are forced rather than chosen: something has to point the linker
        // at the archive, and calling a C symbol is unsafe by definition. What
        // *is* chosen is denying the escape hatch — an unsafe operation hiding
        // inside an `unsafe fn` with no block marking it.
        let crate_ = generated();
        let manifest = crate_.file("Cargo.toml").expect("Cargo.toml");
        assert!(manifest.contains("build = \"build.rs\""), "{manifest}");
        assert!(!manifest.contains("unsafe_code = \"forbid\""), "{manifest}");
        assert!(
            manifest.contains("unsafe_op_in_unsafe_fn = \"deny\""),
            "{manifest}"
        );
    }

    #[test]
    fn the_crate_is_the_same_four_files_every_time() {
        let crate_ = generated();
        for file in ["Cargo.toml", "README.md", "build.rs", "src/lib.rs"] {
            assert!(crate_.file(file).is_some(), "{file} is missing");
        }
        assert_eq!(crate_, generated(), "generation is not deterministic");
    }

    #[test]
    fn the_public_api_is_the_one_the_vm_engine_offers() {
        // The feature's central claim, checked as text: a consumer writing
        // `Uifoundation::load()?` then `ui.make_button("ok")?` compiles against
        // either engine. The engines' *internals* share nothing, which is
        // exactly why this needs pinning rather than assuming.
        let native = native_lib_rs(&model());
        for item in [
            "pub struct Uifoundation",
            "pub struct Button",
            "pub fn load()",
            "pub fn make_button(",
            "pub fn button_width(",
            "pub fn library(&self)",
            "pub type Error = kira_main::Error;",
        ] {
            assert!(native.contains(item), "the native crate has no `{item}`");
        }
    }

    #[test]
    fn a_surface_missing_a_symbol_is_refused_rather_than_left_as_a_hole() {
        // Unreachable when both came from one build, and checked anyway: an
        // `extern` block with a gap in it is a link failure against a symbol
        // nobody named, reported in the consumer's crate.
        let mut broken = symbols();
        broken.functions.clear();
        let error = NativeModel::build("uifoundation", &uifoundation(), &broken)
            .expect_err("a surface with no trampolines");
        assert!(error.to_string().contains("trampoline"), "{error}");
    }
}
