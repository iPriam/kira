//! End-to-end tests for the VM's `CALL_FOREIGN` path.
//!
//! These drive a real `Module` carrying a foreign-import table through the
//! interpreter, with an in-test [`HostCapabilities`] that implements
//! `call_foreign`. They prove the seam without any real sidecar or dynamic
//! loading: the VM marshals its values to the import's exact-width signature,
//! asks the host, absorbs the typed result, and reclaims every argument.

use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::{
    CapturingHost, Execution, ForeignAbi, ForeignArg, ForeignCallError, ForeignImport,
    ForeignResult, ForeignSignature, ForeignType, HostCapabilities,
};

use crate::{VmError, execute};

/// A host with a foreign half. It dispatches on the foreign-import id the
/// [`foreign_imports`] table pins, so every test shares one stable id space.
#[derive(Default)]
struct ForeignHost {
    lines: Vec<String>,
    fail: bool,
}

impl HostCapabilities for ForeignHost {
    fn write_line(&mut self, text: &str) {
        self.lines.push(text.to_owned());
    }

    fn call_foreign(
        &mut self,
        foreign_id: u32,
        args: &[ForeignArg<'_>],
    ) -> Result<ForeignResult, ForeignCallError> {
        if self.fail {
            return Err(ForeignCallError::NoForeignHost);
        }
        match (foreign_id, args) {
            // add(I32, I32) -> I32
            (0, [ForeignArg::I32(a), ForeignArg::I32(b)]) => {
                Ok(ForeignResult::I32(a.wrapping_add(*b)))
            }
            // name_len(CString) -> U64 — borrows the string, copies nothing out.
            (1, [ForeignArg::CString(text)]) => Ok(ForeignResult::U64(text.len() as u64)),
            // origin() -> RawPtr — a non-null opaque word the VM never reads.
            (2, []) => Ok(ForeignResult::RawPtr(0x1234)),
            // bits(RawPtr) -> U64 — hands the opaque word back as its bits.
            (3, [ForeignArg::RawPtr(word)]) => Ok(ForeignResult::U64(*word)),
            // null_origin() -> RawPtr — a null opaque word, still just data.
            (4, []) => Ok(ForeignResult::RawPtr(0)),
            _ => Err(ForeignCallError::NoForeignHost),
        }
    }
}

#[test]
fn a_foreign_host_error_reclaims_arguments_on_a_persistent_heap() {
    // The CString is lowered into the instance heap before the host is called.
    // A host refusal must still unwind the VM stack and frame, rather than
    // leaving that transient copy live in the instance.
    let module = module(
        vec![I::ConstStr(0), I::CallForeign(1), I::Return],
        vec!["kira".to_owned()],
    );
    let mut instance = crate::Instance::load(module).expect("the foreign module validates");
    let mut host = ForeignHost {
        fail: true,
        ..ForeignHost::default()
    };
    assert_eq!(
        instance.call(&mut host, 0, &[]),
        Err(VmError::ForeignCall(ForeignCallError::NoForeignHost))
    );
    assert_eq!(
        instance.stats().current,
        0,
        "a refused call leaves no heap value"
    );
}

/// The foreign-import table every test in this file shares.
fn foreign_imports() -> Vec<ForeignImport> {
    vec![
        ForeignImport::new(
            "ffimath",
            "kira_ffi_add",
            ForeignAbi::C,
            ForeignSignature::scalars([ForeignType::I32, ForeignType::I32], ForeignType::I32),
        ),
        ForeignImport::new(
            "ffimath",
            "kira_ffi_name_len",
            ForeignAbi::C,
            ForeignSignature::scalars([ForeignType::CString], ForeignType::U64),
        ),
        ForeignImport::new(
            "ffimath",
            "kira_ffi_origin",
            ForeignAbi::C,
            ForeignSignature::scalars([], ForeignType::RawPtr),
        ),
        ForeignImport::new(
            "ffimath",
            "kira_ffi_bits",
            ForeignAbi::C,
            ForeignSignature::scalars([ForeignType::RawPtr], ForeignType::U64),
        ),
        ForeignImport::new(
            "ffimath",
            "kira_ffi_null_origin",
            ForeignAbi::C,
            ForeignSignature::scalars([], ForeignType::RawPtr),
        ),
    ]
}

/// A single-`main` module with the shared foreign table and the given code.
fn module(code: Vec<I>, strings: Vec<String>) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: foreign_imports(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: 0,
            execution: Execution::Runtime,
            code,
            releases: kira_bytecode::FrameRelease::EveryLocal,
        }],
        main: Some(0),
        strings,
    }
}

#[test]
fn a_foreign_call_marshals_scalars_through_the_host_and_back() {
    // main: keep(99, add(20, 22)) — the value below the call arguments must
    // survive the crossing and become the function result.
    let module = module(
        vec![
            I::ConstInt(99),
            I::ConstInt(20),
            I::ConstInt(22),
            I::CallForeign(0),
            I::Print,
            I::Pop,
            I::Return,
        ],
        Vec::new(),
    );
    let mut host = ForeignHost::default();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(host.lines, ["42"]);
    assert_eq!(outcome.result, crate::Value::Int(99));
    assert_eq!(outcome.heap.current, 0, "no heap was leaked");
}

#[test]
fn a_vm_only_host_surfaces_the_default_foreign_refusal() {
    // The default `call_foreign` refuses, and that refusal reaches the program
    // as a typed VM error rather than a panic or a wrong answer.
    let module = module(
        vec![
            I::ConstInt(1),
            I::ConstInt(2),
            I::CallForeign(0),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        Vec::new(),
    );
    let mut host = CapturingHost::new();
    let error = execute(&module, &mut host).expect_err("a VM-only host has no foreign half");
    assert_eq!(error, VmError::ForeignCall(ForeignCallError::NoForeignHost));
}

#[test]
fn a_cstring_argument_is_borrowed_and_its_transient_copy_is_reclaimed() {
    // main: print(name_len("kira"))  — the String is copied onto the stack,
    // borrowed as a CString for the one call, and freed on the way out. The
    // U64 length (4) proves the bytes crossed; the balanced heap proves nothing
    // was leaked or double-freed.
    let module = module(
        vec![
            I::ConstStr(0),
            I::CallForeign(1),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        vec!["kira".to_owned()],
    );
    let mut host = ForeignHost::default();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(host.lines, ["4"]);
    assert_eq!(
        outcome.heap.current, 0,
        "the transient CString copy was freed"
    );
}

#[test]
fn a_non_null_raw_pointer_round_trips_without_deref_or_free() {
    // main: print(bits(origin()))  — origin() hands back an opaque word, which
    // the VM stores as a RawPtr value and passes straight back to bits(). The
    // VM never dereferences or frees it; only its bits are observed at the end.
    let module = module(
        vec![
            I::CallForeign(2),
            I::CallForeign(3),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        Vec::new(),
    );
    let mut host = ForeignHost::default();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(host.lines, ["4660"]); // 0x1234
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn a_null_raw_pointer_round_trips_as_plain_data() {
    // A null pointer is data like any other: it round-trips through the VM and
    // back to the host without a dereference or a free.
    let module = module(
        vec![
            I::CallForeign(4),
            I::CallForeign(3),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
        Vec::new(),
    );
    let mut host = ForeignHost::default();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(host.lines, ["0"]);
    assert_eq!(outcome.heap.current, 0);
}
