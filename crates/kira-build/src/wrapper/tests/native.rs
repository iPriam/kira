//! What the **native engine's** generator emits.
//!
//! Split from the VM engine's tests by the file-size ladder, along the line the
//! two already had: this engine emits an `extern` block, `unsafe`, and a
//! `build.rs`, which is a different file rather than a different binding.

use std::path::Path;

use kira_bytecode::{ExportTable, ExportType, ModuleExport};
use kira_llvm_backend::NativeExportSurface;

use super::vm::uifoundation;
use crate::wrapper::render_native::{NativeModel, build_rs, lib_rs as native_lib_rs};
use crate::wrapper::{GeneratedCrate, NativeWrapperSpec, generate_native};

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
