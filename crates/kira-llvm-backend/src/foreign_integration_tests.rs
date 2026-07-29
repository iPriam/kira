//! End-to-end proof of the generated foreign adapter: a Kira program that calls
//! real C symbols is compiled to a native executable, linked against a C static
//! archive, run, and its output checked.
//!
//! This is the one place the whole item-5 path is exercised for real — adapter
//! emission, every supported conversion, and the widened link that pulls the C
//! archive in — rather than inspected. The C fixture is compiled here with the
//! managed clang so the test needs no checked-in binary and no system headers
//! (it uses only builtin types and a hand-rolled `strlen`).

use std::path::{Path, PathBuf};
use std::process::Command;

use kira_ir::IrProgram;
use kira_runtime_abi::{ForeignAbi, ForeignSignature, ForeignType};
use kira_semantics_model::Type;
use kira_semantics_model::hir::{
    Builtin, Callee, ForeignId, HirExpr, HirExprId, HirForeign, HirFunction, HirProgram, HirStmt,
};
use kira_source::Span;

use kira_native_lib_definition::{
    NativeLinkAttributes, NativeLinkInputs, ResolvedTargetRow, TargetTriple,
};

use crate::{NativeBuildOptions, build_native};

/// The link inputs for a build whose one foreign library is `archive`.
///
/// A row is how the resolver hands an archive to the link line, so the test
/// goes through the same shape rather than a path list of its own.
fn link_inputs(archive: &Path) -> NativeLinkInputs {
    let mut inputs = NativeLinkInputs::default();
    inputs.push_row(&ResolvedTargetRow::new(
        TargetTriple::new("test", "test", "none"),
        Some(archive.to_path_buf()),
        NativeLinkAttributes::default(),
    ));
    inputs
}

/// The C fixture: one function per supported conversion, no system headers.
const FIXTURE_C: &str = r#"
signed char kt_neg_i8(signed char x) { return (signed char)(-x); }
unsigned char kt_wrap_u8(unsigned char x) { return x; }
short kt_neg_i16(short x) { return (short)(-x); }
int kt_add_i32(int a, int b) { return a + b; }
long long kt_add_i64(long long a, long long b) { return a + b; }
_Bool kt_not(_Bool b) { return !b; }
float kt_add_f32(float a, float b) { return a + b; }
double kt_add_f64(double a, double b) { return a + b; }
unsigned long long kt_strlen(const char* s) {
    unsigned long long n = 0;
    while (s[n]) n++;
    return n;
}
void* kt_make_ptr(void) { return (void*)42; }
long long kt_ptr_word(void* p) { return (long long)p; }
"#;

/// The output the program prints, one line per foreign call, in body order.
const EXPECTED_OUTPUT: &str = "42\n-5\n200\n-9\n1975\nfalse\n3.75\n1.75\n4\n42\n";

/// A foreign import row for the HIR.
fn foreign(symbol: &str, params: &[ForeignType], result: ForeignType) -> HirForeign {
    HirForeign {
        kira_name: symbol.to_owned(),
        library: "ffitest".to_owned(),
        symbol: symbol.to_owned(),
        abi: ForeignAbi::C,
        signature: ForeignSignature::scalars(params.iter().copied(), result),
        param_pointees: Box::new([]),
        param_wrappers: params.iter().map(|_| None).collect(),
        result_wrapper: None,
        name_span: Span::new(0, 0),
    }
}

/// Builds the IR for a program calling every fixture function and printing each
/// result.
fn fixture_program() -> IrProgram {
    let mut program = HirProgram {
        foreign: foreign_table(),
        ..HirProgram::default()
    };

    let mut body = Vec::new();
    build_body(&mut program, &mut body);

    let ret = program.stmts.alloc(HirStmt::Return { value: None });
    body.push(ret);

    program.functions.push(HirFunction {
        name: "main".to_owned(),
        param_count: 0,
        return_type: Type::Void,
        locals: Vec::new(),
        body,
        is_main: true,
        is_async: false,
        execution: kira_runtime_abi::Execution::Inherited,
        mutates_self: false,
        name_span: Span::new(0, 4),
    });
    program.main = Some(kira_semantics_model::hir::FuncId(0));
    kira_ir::lower(&program)
}

/// The foreign import table, one row per fixture function.
fn foreign_table() -> Vec<HirForeign> {
    vec![
        foreign(
            "kt_add_i32",
            &[ForeignType::I32, ForeignType::I32],
            ForeignType::I32,
        ),
        foreign("kt_neg_i8", &[ForeignType::I8], ForeignType::I8),
        foreign("kt_wrap_u8", &[ForeignType::U8], ForeignType::U8),
        foreign("kt_neg_i16", &[ForeignType::I16], ForeignType::I16),
        foreign(
            "kt_add_i64",
            &[ForeignType::I64, ForeignType::I64],
            ForeignType::I64,
        ),
        foreign("kt_not", &[ForeignType::Bool], ForeignType::Bool),
        foreign(
            "kt_add_f32",
            &[ForeignType::F32, ForeignType::F32],
            ForeignType::F32,
        ),
        foreign(
            "kt_add_f64",
            &[ForeignType::F64, ForeignType::F64],
            ForeignType::F64,
        ),
        foreign("kt_strlen", &[ForeignType::CString], ForeignType::U64),
        foreign("kt_make_ptr", &[], ForeignType::RawPtr),
        foreign("kt_ptr_word", &[ForeignType::RawPtr], ForeignType::I64),
    ]
}

/// Appends one `print(<foreign call>)` statement per fixture function.
fn build_body(program: &mut HirProgram, body: &mut Vec<kira_semantics_model::hir::HirStmtId>) {
    // print(kt_add_i32(20, 22)) -> 42
    body.push(call_print(
        program,
        0,
        &[HirExpr::Int(20), HirExpr::Int(22)],
        Type::INT,
    ));
    // print(kt_neg_i8(5)) -> -5 (sign extension)
    body.push(call_print(program, 1, &[HirExpr::Int(5)], Type::INT));
    // print(kt_wrap_u8(200)) -> 200 (zero extension, not -56)
    body.push(call_print(program, 2, &[HirExpr::Int(200)], Type::INT));
    // print(kt_neg_i16(9)) -> -9
    body.push(call_print(program, 3, &[HirExpr::Int(9)], Type::INT));
    // print(kt_add_i64(1000, 975)) -> 1975
    body.push(call_print(
        program,
        4,
        &[HirExpr::Int(1000), HirExpr::Int(975)],
        Type::INT,
    ));
    // print(kt_not(true)) -> false
    body.push(call_print(program, 5, &[HirExpr::Bool(true)], Type::Bool));
    // print(kt_add_f32(1.5, 2.25)) -> 3.75
    body.push(call_print(
        program,
        6,
        &[HirExpr::Float(1.5), HirExpr::Float(2.25)],
        Type::FLOAT,
    ));
    // print(kt_add_f64(1.5, 0.25)) -> 1.75
    body.push(call_print(
        program,
        7,
        &[HirExpr::Float(1.5), HirExpr::Float(0.25)],
        Type::FLOAT,
    ));
    // print(kt_strlen("kira")) -> 4 (transient CString)
    body.push(call_print(
        program,
        8,
        &[HirExpr::Str("kira".to_owned())],
        Type::INT,
    ));
    // print(kt_ptr_word(kt_make_ptr())) -> 42 (RawPtr round-trip)
    let make_ptr = program.exprs.alloc(HirExpr::Call {
        callee: Callee::Foreign(ForeignId(9)),
        args: vec![],
        ty: Type::RawPtr,
        writebacks: Vec::new(),
    });
    let ptr_word = program.exprs.alloc(HirExpr::Call {
        callee: Callee::Foreign(ForeignId(10)),
        args: vec![make_ptr],
        ty: Type::INT,
        writebacks: Vec::new(),
    });
    body.push(print_stmt(program, ptr_word, Type::INT));
}

/// Allocates `print(<foreign call>)` and returns its statement id.
fn call_print(
    program: &mut HirProgram,
    foreign_index: u32,
    args: &[HirExpr],
    result_ty: Type,
) -> kira_semantics_model::hir::HirStmtId {
    let arg_ids: Vec<HirExprId> = args
        .iter()
        .map(|expr| program.exprs.alloc(expr.clone()))
        .collect();
    let call = program.exprs.alloc(HirExpr::Call {
        callee: Callee::Foreign(ForeignId(foreign_index)),
        args: arg_ids,
        ty: result_ty,
        writebacks: Vec::new(),
    });
    print_stmt(program, call, result_ty)
}

/// Allocates `print(<value>)` for a value of `ty` and returns its statement id.
fn print_stmt(
    program: &mut HirProgram,
    value: HirExprId,
    ty: Type,
) -> kira_semantics_model::hir::HirStmtId {
    let _ = ty;
    let print = program.exprs.alloc(HirExpr::Call {
        callee: Callee::Builtin(Builtin::Print),
        args: vec![value],
        ty: Type::Void,
        writebacks: Vec::new(),
    });
    program.stmts.alloc(HirStmt::Expr { expr: print })
}

/// Locates `libkira_native_bridge.a`, built as a workspace staticlib, by
/// searching the profile directory the test binary lives under.
fn runtime_archive() -> PathBuf {
    let exe = std::env::current_exe().expect("test binary has a path");
    for ancestor in exe.ancestors() {
        let candidate = ancestor.join("libkira_native_bridge.a");
        if candidate.is_file() {
            return candidate;
        }
    }
    panic!(
        "libkira_native_bridge.a not found near {}; run under `cargo test --workspace`",
        exe.display()
    );
}

/// Compiles the C fixture into `lib<name>.a` in `dir` using the managed clang.
fn build_fixture_archive(dir: &Path, name: &str) -> PathBuf {
    let llvm = kira_toolchain::discover(None).expect("managed LLVM is present");
    let source = dir.join("fixture.c");
    std::fs::write(&source, FIXTURE_C).expect("fixture source is writable");
    let object = dir.join("fixture.o");
    let compile = Command::new(llvm.clang())
        .arg("-c")
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("clang runs");
    assert!(
        compile.status.success(),
        "compiling the C fixture failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let archive = dir.join(format!("lib{name}.a"));
    let ar = Command::new(llvm.llvm_ar())
        .arg("crs")
        .arg(&archive)
        .arg(&object)
        .output()
        .expect("llvm-ar runs");
    assert!(
        ar.status.success(),
        "archiving the C fixture failed: {}",
        String::from_utf8_lossy(&ar.stderr)
    );
    archive
}

#[test]
fn a_native_program_calls_c_symbols_through_generated_adapters() {
    let dir = std::env::temp_dir().join(format!("kira-ffi-adapter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir is creatable");
    let archive = build_fixture_archive(&dir, "ffitest");

    let program = fixture_program();
    assert_eq!(program.foreign_imports.len(), 11);

    let executable = dir.join("ffi_program");
    let artifacts = build_native(
        &program,
        &NativeBuildOptions {
            module_name: "ffi_program".to_owned(),
            object_path: dir.join("ffi_program.o"),
            executable_path: Some(executable.clone()),
            shared_library_path: None,
            archive_path: None,
            exports: crate::NativeExportSurface::default(),
            ir_path: None,
            runtime_archive: runtime_archive(),
            foreign_link: link_inputs(&archive),
            optimize: false,
            unavailable_imports: Vec::new(),
        },
    )
    .expect("the FFI program links");
    assert_eq!(artifacts.executable.as_deref(), Some(executable.as_path()));

    let run = Command::new(&executable)
        .output()
        .expect("the program runs");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "the FFI program exited with failure: {}\nstdout:\n{stdout}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(stdout, EXPECTED_OUTPUT, "adapter output mismatch");

    let _ = std::fs::remove_dir_all(&dir);
}
