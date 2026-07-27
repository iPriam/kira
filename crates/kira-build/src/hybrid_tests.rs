//! Tests for the hybrid library build.
//!
//! Split from `hybrid.rs` rather than sitting at its foot, per this workspace's
//! file-size ladder.
//!
//! Everything here runs **without LLVM**, which is deliberate: the two refusals
//! and the manifest are what a hybrid library build decides before it reaches a
//! backend, and they are the parts CI can prove. Building the native half is
//! LLVM-gated and proved by the consumer crate.

use super::*;
use crate::wrapper;
use crate::wrapper_export_name;

use kira_ir::{IrCallee, IrExpr, IrExprId, IrFunction, IrStmt};
use la_arena::Arena;

/// A program of `functions`, sharing one expression arena.
fn program(
    functions: Vec<IrFunction>,
    exprs: Arena<IrExpr>,
    exports: Vec<kira_ir::IrExport>,
) -> IrProgram {
    IrProgram {
        functions,
        types: Default::default(),
        main: None,
        exports,
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        exprs,
    }
}

/// One function with no parameters, returning `Void`.
fn function(name: &str, execution: Execution, body: Vec<IrStmt>) -> IrFunction {
    IrFunction {
        name: name.to_owned(),
        param_count: 0,
        locals: Vec::new(),
        native_state_locals: Vec::new(),
        return_type: Type::Void,
        execution,
        by_reference_params: Vec::new(),
        by_pointer_params: Vec::new(),
        body,
    }
}

/// A statement calling user function `index` for effect.
fn call(exprs: &mut Arena<IrExpr>, index: u32) -> IrStmt {
    let expr: IrExprId = exprs.alloc(IrExpr::Call {
        callee: IrCallee::User(index),
        args: Vec::new(),
        result: Type::Void,
        writebacks: Vec::new(),
    });
    IrStmt::Eval { expr }
}

/// An export of function `index`.
fn export(kira_name: &str, index: u32) -> kira_ir::IrExport {
    kira_ir::IrExport {
        kira_name: kira_name.to_owned(),
        exported_name: wrapper_export_name(kira_name),
        function: index,
        params: Vec::new(),
        result: Type::Void,
    }
}

#[test]
fn an_annotation_free_library_builds_its_split_with_everything_on_the_vm() {
    // The default matters: a library that says nothing about engines is a
    // bytecode library that happens to carry a manifest, and nothing about
    // building it for this engine may quietly move a function.
    let exprs = Arena::new();
    let ir = program(
        vec![function("hidden", Execution::Inherited, Vec::new())],
        exprs,
        Vec::new(),
    );
    assert_eq!(engines(&ir), vec![Execution::Runtime]);
    check_library(&ir).expect("nothing to refuse");
}

#[test]
fn a_native_function_a_runtime_function_calls_is_the_direction_that_works() {
    // The whole point of this engine: the surface stays on the VM and the hot
    // function underneath is machine code. Runtime-calls-native is the crossing
    // the seam already serves, so it must not be caught by the refusal aimed at
    // the other direction.
    let mut exprs = Arena::new();
    let body = vec![call(&mut exprs, 1)];
    let ir = program(
        vec![
            function("surface", Execution::Runtime, body),
            function("hot", Execution::Native, Vec::new()),
        ],
        exprs,
        vec![export("surface", 0)],
    );
    check_library(&ir).expect("runtime calling native is the supported direction");
}

#[test]
fn a_native_function_calling_a_runtime_function_is_refused_naming_both() {
    let mut exprs = Arena::new();
    let body = vec![call(&mut exprs, 1)];
    let ir = program(
        vec![
            function("hot", Execution::Native, body),
            function("helper", Execution::Runtime, Vec::new()),
        ],
        exprs,
        Vec::new(),
    );
    let error = check_library(&ir).expect_err("a library cannot be re-entered");
    let message = error.to_string();
    assert!(message.contains("hot"), "{message}");
    assert!(message.contains("helper"), "{message}");
    // The refusal has to say what an application may do that a library may not,
    // or it reads as a bug in the seam rather than as a limit of this shape.
    assert!(message.contains("application"), "{message}");
}

#[test]
fn a_native_function_calling_another_native_function_is_fine() {
    // Machine code calling machine code never touches the instance, so it is
    // not the thing the refusal is about — and a refusal that caught it would
    // make the annotation useless for anything but a leaf.
    let mut exprs = Arena::new();
    let body = vec![call(&mut exprs, 1)];
    let ir = program(
        vec![
            function("outer", Execution::Native, body),
            function("inner", Execution::Native, Vec::new()),
        ],
        exprs,
        Vec::new(),
    );
    check_library(&ir).expect("native calling native stays inside the library");
}

#[test]
fn a_call_buried_inside_a_loop_inside_a_branch_is_still_found() {
    // The walk is the refusal. A call the walk misses is a program that builds
    // and then aborts at run time naming a null invoker, which is exactly the
    // failure this check exists to convert into a build error.
    let mut exprs = Arena::new();
    let inner = call(&mut exprs, 1);
    let cond = exprs.alloc(IrExpr::Bool(true));
    let body = vec![IrStmt::If {
        cond,
        then_body: vec![IrStmt::While {
            cond,
            body: vec![inner],
        }],
        else_body: Vec::new(),
    }];
    let ir = program(
        vec![
            function("hot", Execution::Native, body),
            function("helper", Execution::Runtime, Vec::new()),
        ],
        exprs,
        Vec::new(),
    );
    let error = check_library(&ir).expect_err("depth does not hide a call");
    assert!(error.to_string().contains("helper"), "{error}");
}

#[test]
fn an_export_that_is_also_native_is_refused_by_name() {
    let exprs = Arena::new();
    let ir = program(
        vec![function("makeButton", Execution::Native, Vec::new())],
        exprs,
        vec![export("makeButton", 0)],
    );
    let error = check_library(&ir).expect_err("an export enters the bytecode half");
    let message = error.to_string();
    assert!(message.contains("makeButton"), "{message}");
    assert!(message.contains("@Export"), "{message}");
    assert!(message.contains("@Native"), "{message}");
}

#[test]
fn the_manifest_records_a_library_as_having_no_entrypoint() {
    // The `.khm` format already had the sentinel; this is the case it was for.
    // A library that decoded as having an entrypoint would be a bundle the
    // runtime would try to `run()`.
    let exprs = Arena::new();
    let ir = program(
        vec![function("surface", Execution::Runtime, Vec::new())],
        exprs,
        Vec::new(),
    );
    let described = manifest(
        &ir,
        "uifoundation",
        "uifoundation.kbc",
        "libuifoundation.dylib",
        &[],
    )
    .expect("describe");
    assert_eq!(described.entry, None);
    assert_eq!(described.functions.len(), 1);
    assert_eq!(described.functions[0].execution, Execution::Runtime);
    assert_eq!(described.functions[0].exported_name, None);
}

#[test]
fn the_manifest_records_the_symbol_the_backend_emitted_rather_than_deriving_one() {
    let exprs = Arena::new();
    let ir = program(
        vec![
            function("surface", Execution::Runtime, Vec::new()),
            function("hot", Execution::Native, Vec::new()),
        ],
        exprs,
        Vec::new(),
    );
    let described = manifest(
        &ir,
        "uifoundation",
        "uifoundation.kbc",
        "libuifoundation.dylib",
        &[(1, "kira_native_fn_1".to_owned())],
    )
    .expect("describe");
    assert_eq!(described.functions[0].exported_name, None);
    assert_eq!(
        described.functions[1].exported_name.as_deref(),
        Some("kira_native_fn_1")
    );
}

#[test]
fn the_manifest_round_trips_through_its_own_bytes() {
    // A `.khm` is embedded in the generated crate and decoded by the consumer,
    // so what this build writes has to be what a decoder reads back.
    let exprs = Arena::new();
    let ir = program(
        vec![function("surface", Execution::Runtime, Vec::new())],
        exprs,
        Vec::new(),
    );
    let described = manifest(
        &ir,
        "uifoundation",
        "uifoundation.kbc",
        "libuifoundation.dylib",
        &[],
    )
    .expect("describe");
    let bytes = described.to_bytes();
    let decoded = HybridManifest::from_bytes(&bytes).expect("decode");
    assert_eq!(decoded, described);
}

#[test]
fn the_embedded_manifest_is_named_beside_the_bytecode() {
    // Both are read by `include_bytes!("../<file>")` from the generated
    // `src/lib.rs`, so both live at the crate root under the library's name.
    assert_eq!(
        wrapper::manifest_file_name("uifoundation"),
        "uifoundation.khm"
    );
    assert_eq!(
        wrapper::artifact_file_name("uifoundation"),
        "uifoundation.kbc"
    );
}

#[test]
fn a_hybrid_build_removes_the_native_engines_build_script() {
    // Same hazard the VM engine's build has, for the same reason: cargo runs a
    // build script it *finds*, so a `build.rs` surviving from
    // `--backend llvm` would make this crate link a stale archive it does not
    // otherwise reference.
    let foreign = wrapper::foreign_engine_files(wrapper::Engine::Hybrid, "uifoundation");
    assert!(foreign.contains(&PathBuf::from("build.rs")), "{foreign:?}");
    // And it keeps its own two embedded artifacts.
    assert!(!foreign.contains(&PathBuf::from("uifoundation.kbc")));
    assert!(!foreign.contains(&PathBuf::from("uifoundation.khm")));
}

#[test]
fn switching_to_another_engine_removes_this_ones_manifest() {
    // The other direction of the same hazard: a `.khm` describing a split, left
    // beside a `.kbc` that has none, is a file that says the wrong thing about
    // what is in the directory.
    for engine in [wrapper::Engine::Vm, wrapper::Engine::Native] {
        let foreign = wrapper::foreign_engine_files(engine, "uifoundation");
        assert!(
            foreign.contains(&PathBuf::from("uifoundation.khm")),
            "{engine:?}: {foreign:?}"
        );
    }
}

#[test]
fn every_type_a_v1_signature_can_have_gets_a_bridge_tag() {
    // The manifest has a row for every function in the program, most of which
    // never cross, so a type that cannot *travel* still has to be describable.
    // Moved here with the manifest builder itself: one description, one test.
    let described = manifest(
        &program(
            vec![IrFunction {
                name: "every".to_owned(),
                param_count: 5,
                locals: vec![Type::INT, Type::FLOAT, Type::Bool, Type::String, Type::Void],
                native_state_locals: vec![None; 5],
                return_type: Type::Void,
                execution: Execution::Runtime,
                by_reference_params: Vec::new(),
                by_pointer_params: Vec::new(),
                body: Vec::new(),
            }],
            Arena::new(),
            Vec::new(),
        ),
        "demo",
        "demo.kbc",
        "libdemo.dylib",
        &[],
    )
    .expect("describe");
    let tags: Vec<_> = described.functions[0]
        .params
        .iter()
        .map(|param| param.ty)
        .collect();
    assert_eq!(
        tags,
        [
            BridgeValueTag::INT,
            BridgeValueTag::FLOAT,
            BridgeValueTag::BOOL,
            BridgeValueTag::STRING,
            BridgeValueTag::VOID,
        ]
    );
}

#[test]
fn the_error_type_is_refused_rather_than_encoded() {
    // A verified IR carries no `Error` type, so reaching one means the frontend
    // let a broken program through. Refused rather than written into an artifact
    // that would then decode as something.
    let ir = program(
        vec![IrFunction {
            name: "broken".to_owned(),
            param_count: 0,
            locals: Vec::new(),
            native_state_locals: Vec::new(),
            return_type: Type::Error,
            execution: Execution::Runtime,
            by_reference_params: Vec::new(),
            by_pointer_params: Vec::new(),
            body: Vec::new(),
        }],
        Arena::new(),
        Vec::new(),
    );
    let error = manifest(&ir, "demo", "demo.kbc", "libdemo.dylib", &[])
        .expect_err("the error type cannot be described");
    assert!(
        matches!(
            error,
            HybridLibraryError::UnsupportedType {
                ty: Type::Error,
                ..
            }
        ),
        "{error:?}"
    );
}
