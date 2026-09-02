//! Autobind proved against real headers and a real clang.
//!
//! The generator's whole job is to agree with the C compiler, so these tests
//! write C, run the managed toolchain's clang over it, and read the Kira that
//! comes out. A test that stubbed the parser would prove the emitter and
//! nothing about the mapping, which is where every interesting mistake lives.

use std::path::{Path, PathBuf};

use kira_native_lib_definition::{
    AutobindMode, AutobindSpec, LinkMode, NativeHeaders, NativeLibrarySpec, NativeTargetSpec,
    TargetTriple,
};

use super::*;

/// A scratch package that removes itself.
struct TempPackage(PathBuf);

impl TempPackage {
    fn new(tag: &str) -> Self {
        let base = std::env::temp_dir().join(format!(
            "kira-autobind-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("NativeLibs")).expect("a scratch package");
        std::fs::create_dir_all(base.join("app")).expect("a scratch source root");
        TempPackage(base)
    }

    fn header(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join("NativeLibs").join(name);
        std::fs::write(&path, text).expect("write a header");
        path
    }

    fn context(&self) -> AutobindContext {
        AutobindContext {
            package_root: self.0.clone(),
            source_root: self.0.join("app"),
            base_dir: self.0.clone(),
            target: host_target(),
        }
    }
}

impl Drop for TempPackage {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A library declaring one header, bound in full, with a row for this host.
fn library(headers: &[&str]) -> NativeLibrarySpec {
    let row = NativeTargetSpec::static_archive(host_target(), "lib/libdemo.a");
    let mut spec = NativeLibrarySpec::new("demo", LinkMode::Static, vec![row])
        .expect("a well-formed declaration");
    spec = spec.with_headers(NativeHeaders {
        entrypoint: headers.first().map(|name| format!("NativeLibs/{name}")),
        include_dirs: vec!["NativeLibs".to_owned()],
        defines: Vec::new(),
    });
    spec.with_autobind(AutobindSpec {
        module: Some("demo".to_owned()),
        headers: headers
            .iter()
            .map(|name| format!("NativeLibs/{name}"))
            .collect(),
        mode: AutobindMode::AllPublic,
        ..AutobindSpec::default()
    })
}

/// Generates `spec` in `package` and hands back the binding's text.
fn bind(package: &TempPackage, spec: &NativeLibrarySpec) -> String {
    let llvm = kira_toolchain::discover(None).expect("the managed LLVM bundle");
    let clang = Clang::load(&llvm.home).expect("libclang loads out of the bundle");
    let plan = plan(spec, &package.context())
        .expect("a resolvable declaration")
        .expect("a library with headers and a row for this host");
    assert_eq!(plan.status, AutobindStatus::Stale);
    let report = generate(&plan, spec, &clang).expect("generation");
    assert_eq!(report.output, plan.output);
    std::fs::read_to_string(&plan.output).expect("the generated binding")
}

#[test]
fn a_function_and_the_types_its_signature_reaches_are_bound() {
    let package = TempPackage::new("signature");
    package.header(
        "demo.h",
        "typedef struct demo_engine demo_engine;\n\
         typedef struct { float ascender; float descender; } demo_metrics;\n\
         demo_engine *demo_open(const char *path, int index);\n\
         void demo_measure(demo_engine *engine, demo_metrics *out);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains(
            "@FFI.Extern { library: demo, symbol: demo_open, abi: c }\n\
             function demo_open(path: CString, index: I32) -> demo_engine_ptr"
        ),
        "{text}"
    );
    assert!(
        text.contains("@FFI.Pointer { target: demo_engine, ownership: borrowed }"),
        "{text}"
    );
    assert!(
        text.contains("@FFI.Alias { target: demo_engine }"),
        "an opaque C type gets an alias so its pointer has a target: {text}"
    );
    assert!(
        text.contains("@FFI.Struct { layout: c }\nstruct demo_metrics {\n    var ascender: F32"),
        "{text}"
    );
}

#[test]
fn a_struct_forward_declared_before_it_is_defined_keeps_its_fields() {
    let package = TempPackage::new("forward");
    package.header(
        "demo.h",
        "struct demo_caps;\n\
         typedef void (*demo_probe)(struct demo_caps *caps);\n\
         typedef struct demo_caps { int count; float scale; } demo_caps;\n\
         void demo_release(demo_caps caps);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains("@FFI.Struct { layout: c }\nstruct demo_caps {\n    var count: I32"),
        "a type the header defines is a struct even when a forward declaration \
         named it first: {text}"
    );
    assert!(
        !text.contains("@FFI.Alias { target: demo_caps }"),
        "a defined type must not also be declared as an opaque handle, which \
         would alias to itself and have no layout to pass by value: {text}"
    );
}

#[test]
fn a_handle_typedef_binds_the_functions_that_take_it() {
    let package = TempPackage::new("handle");
    package.header(
        "demo.h",
        "typedef struct demo_deviceImpl* demo_device;\n\
         demo_device demo_device_create(void);\n\
         void demo_device_release(demo_device device);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains("function demo_device_release(device: demo_deviceImpl_ptr) -> Void"),
        "a pointer the header typedef'd is still a pointer to a named type: {text}"
    );
    assert!(
        text.contains("@FFI.Alias { target: demo_deviceImpl }"),
        "the handle's pointee is declared so the pointer has a target: {text}"
    );
}

/// The same one-header declaration, narrowed to named functions. This is the
/// mode where reachable C-layout types must be discovered from signatures
/// rather than from the header's standalone type declarations.
fn selected_library(headers: &[&str], functions: &[&str]) -> NativeLibrarySpec {
    let row = NativeTargetSpec::static_archive(host_target(), "lib/libdemo.a");
    let mut spec = NativeLibrarySpec::new("demo", LinkMode::Static, vec![row])
        .expect("a well-formed declaration");
    spec = spec.with_headers(NativeHeaders {
        entrypoint: headers.first().map(|name| format!("NativeLibs/{name}")),
        include_dirs: vec!["NativeLibs".to_owned()],
        defines: Vec::new(),
    });
    spec.with_autobind(AutobindSpec {
        module: Some("demo".to_owned()),
        headers: headers
            .iter()
            .map(|name| format!("NativeLibs/{name}"))
            .collect(),
        functions: functions.iter().map(|name| (*name).to_owned()).collect(),
        mode: AutobindMode::Selected,
        ..AutobindSpec::default()
    })
}

#[test]
fn nested_pointers_keep_each_c_pointer_layer_in_the_generated_binding() {
    let package = TempPackage::new("nested-pointers");
    package.header(
        "demo.h",
        "typedef struct demo_engine demo_engine;\n\
         int demo_create(demo_engine **out);\n\
         void demo_destroy(demo_engine *engine);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains(
            "@FFI.Pointer { target: demo_engine, ownership: borrowed }\n\
             struct demo_engine_ptr {}"
        ),
        "the first pointer layer is named: {text}"
    );
    assert!(
        text.contains(
            "@FFI.Pointer { target: demo_engine_ptr, ownership: borrowed }\n\
             struct demo_engine_ptr_ptr {}"
        ),
        "the out-parameter pointer layer is named rather than skipped: {text}"
    );
    assert!(
        text.contains("function demo_create(out: demo_engine_ptr_ptr) -> I32"),
        "the function keeps the nested pointer type: {text}"
    );
}

#[test]
fn selected_functions_pull_defined_pointer_targets_into_the_binding() {
    let package = TempPackage::new("reachable-record");
    package.header(
        "demo.h",
        "typedef struct demo_row { void *data; unsigned int size; } demo_row;\n\
         int demo_read(demo_row *row);\n",
    );
    let text = bind(&package, &selected_library(&["demo.h"], &["demo_read"]));

    assert!(
        text.contains(
            "@FFI.Struct { layout: c }\nstruct demo_row {\n    var data: RawPtr\n    var size: U32\n}"
        ),
        "a defined record reached only through a selected pointer parameter is emitted: {text}"
    );
    assert!(
        text.contains("function demo_read(row: demo_row_ptr) -> I32"),
        "the pointer still carries the record target: {text}"
    );
}

#[test]
fn an_inline_array_becomes_an_ffi_array_typedef_named_for_its_storage() {
    let package = TempPackage::new("array");
    package.header(
        "demo.h",
        "typedef struct { int sig; char opaque[56]; } demo_lock;\n\
         void demo_take(demo_lock lock);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    // Plain `char` takes its signedness from the target — signed on x86-64,
    // unsigned on aarch64 — and the binding reports the target's, so the
    // element name follows it rather than one platform's.
    let byte = plain_char_spelling();
    assert!(
        text.contains(&format!("@FFI.Array {{ element: {byte}, count: 56 }}")),
        "{text}"
    );
    assert!(text.contains(&format!("var opaque: {byte}_array_56")), "{text}");
}

#[test]
fn a_function_pointer_becomes_a_callback_named_for_its_typedef() {
    let package = TempPackage::new("callback");
    package.header(
        "demo.h",
        "typedef int (*demo_hook)(int, void *);\n\
         void demo_install(demo_hook hook);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains("@FFI.Callback { abi: c, params: [I32, RawPtr], result: I32 }"),
        "{text}"
    );
    assert!(
        text.contains("function demo_install(hook: demo_hook) -> Void"),
        "{text}"
    );
}

#[test]
fn integer_width_is_read_from_the_target_rather_than_the_keyword() {
    let package = TempPackage::new("widths");
    package.header(
        "demo.h",
        "void demo_widths(char a, short b, int c, long d, unsigned long long e, _Bool f);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    let long_spelling = match std::env::consts::OS {
        "windows" => "I32",
        _ => "Int",
    };
    let byte = plain_char_spelling();
    assert!(
        text.contains(&format!(
            "function demo_widths(a: {byte}, b: I16, c: I32, d: {long_spelling}, e: U64, f: Bool) \
             -> Void"
        )),
        "{text}"
    );
}

/// The Kira type a plain C `char` binds to on this host.
///
/// `char` is a third type beside `signed char` and `unsigned char`, and which
/// of the two it matches is the target's choice: x86-64 signs it, aarch64 does
/// not. The binding reports what the target says, so a test that pinned one
/// spelling would pass on one machine and fail on the other for a binding that
/// is right on both.
fn plain_char_spelling() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" | "arm" => "U8",
        _ => "I8",
    }
}

#[test]
fn a_declaration_the_seam_cannot_carry_is_written_down_with_its_reason() {
    let package = TempPackage::new("skipped");
    package.header(
        "demo.h",
        "int demo_printf(const char *format, ...);\n\
         long double demo_precise(void);\n\
         int demo_plain(int value);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(
        text.contains("function demo_plain(value: I32) -> I32"),
        "{text}"
    );
    assert!(
        text.contains("// demo_printf: a variadic C function has no fixed signature to bind"),
        "{text}"
    );
    assert!(
        text.contains("// demo_precise: its result is a `long double`"),
        "{text}"
    );
}

#[test]
fn only_the_declared_headers_contribute_functions() {
    let package = TempPackage::new("scope");
    package.header("other.h", "int other_helper(int value);\n");
    package.header(
        "demo.h",
        "#include \"other.h\"\nint demo_entry(int value);\n",
    );
    let text = bind(&package, &library(&["demo.h"]));

    assert!(text.contains("symbol: demo_entry"), "{text}");
    assert!(
        !text.contains("symbol: other_helper"),
        "an included header's functions belong to whoever declared it: {text}"
    );
}

#[test]
fn a_second_run_finds_the_binding_current_and_an_edited_header_makes_it_stale() {
    let package = TempPackage::new("cache");
    let header = package.header("demo.h", "int demo_entry(int value);\n");
    let spec = library(&["demo.h"]);
    bind(&package, &spec);

    let after = plan(&spec, &package.context())
        .expect("a resolvable declaration")
        .expect("a planned library");
    assert_eq!(after.status, AutobindStatus::Current);

    std::fs::write(
        &header,
        "int demo_entry(int value);\nint demo_more(void);\n",
    )
    .expect("edit the header");
    let edited = plan(&spec, &package.context())
        .expect("a resolvable declaration")
        .expect("a planned library");
    assert_eq!(edited.status, AutobindStatus::Stale);
}

#[test]
fn a_binding_this_generator_did_not_write_is_adopted_rather_than_overwritten() {
    let package = TempPackage::new("adopt");
    package.header("demo.h", "int demo_entry(int value);\n");
    let spec = library(&["demo.h"]);
    let context = package.context();
    let shipped = context.source_root.join("bindings").join("demo.kira");
    std::fs::create_dir_all(shipped.parent().expect("a bindings directory"))
        .expect("create the bindings directory");
    std::fs::write(&shipped, "// shipped by the package\n").expect("write a shipped binding");

    let planned = plan(&spec, &context)
        .expect("a resolvable declaration")
        .expect("a planned library");
    assert_eq!(planned.status, AutobindStatus::Adopt);
    adopt(&planned).expect("adopting writes only the stamp");
    assert_eq!(
        std::fs::read_to_string(&shipped).expect("the shipped binding"),
        "// shipped by the package\n",
        "adopting must not rewrite a file the package ships"
    );

    let after = plan(&spec, &context)
        .expect("a resolvable declaration")
        .expect("a planned library");
    assert_eq!(after.status, AutobindStatus::Current);
}

#[test]
fn a_library_with_no_row_for_this_target_is_not_bound() {
    let package = TempPackage::new("excluded");
    package.header("demo.h", "int demo_entry(int value);\n");
    let mut context = package.context();
    context.target = TargetTriple::new("wasm32", "emscripten", "unknown");
    assert!(
        plan(&library(&["demo.h"]), &context)
            .expect("a resolvable declaration")
            .is_none()
    );
}

#[test]
fn a_declared_output_outside_the_source_root_falls_back_to_the_one_that_compiles() {
    let package = TempPackage::new("output");
    let context = package.context();
    assert_eq!(
        output_path(Some("../elsewhere/demo.kira"), "demo", &context),
        context.source_root.join("bindings").join("demo.kira")
    );
    assert_eq!(
        output_path(Some("app/bindings/other.kira"), "demo", &context),
        context.base_dir.join("app/bindings/other.kira")
    );
}

#[test]
fn an_unset_environment_variable_reads_as_an_sdk_that_is_not_here() {
    assert_eq!(
        expand_environment("plain/path.h"),
        Some("plain/path.h".to_owned())
    );
    assert_eq!(
        expand_environment("${KIRA_AUTOBIND_TEST_UNSET_SDK}/include/x.h"),
        None
    );
}

#[test]
fn a_missing_declared_header_is_named_rather_than_silently_binding_nothing() {
    let package = TempPackage::new("missing");
    let error =
        plan(&library(&["absent.h"]), &package.context()).expect_err("a missing header is a fault");
    let AutobindError::MissingHeader { path, .. } = &error else {
        panic!("expected a missing-header error, got {error}");
    };
    assert!(path.ends_with("absent.h"), "{path}");
}

/// The one path that is not about C at all: a normalized comparison is what
/// decides whether a declared output can be compiled.
#[test]
fn normalization_resolves_a_climb_out_of_a_directory() {
    assert_eq!(
        normalize(Path::new("/pkg/NativeLibs/../bindings/x.kira")),
        PathBuf::from("/pkg/bindings/x.kira")
    );
}
