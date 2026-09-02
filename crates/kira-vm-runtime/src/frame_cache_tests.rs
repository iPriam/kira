//! Regression coverage for reusing returned call frames.
//!
//! A cached frame must be indistinguishable from a fresh one. In particular,
//! dropping a heap value and retaining a scalar release plan must both leave
//! every local at `Void` before a different function shape reuses the storage.

use kira_bytecode::FrameRelease;
use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::{CapturingHost, Execution};

use crate::{Value, execute};

fn function(name: &str, locals: u64, code: Vec<I>, releases: FrameRelease) -> FuncProto {
    FuncProto {
        name: name.to_owned(),
        param_count: 0,
        local_count: locals,
        execution: Execution::Runtime,
        code,
        releases,
    }
}

#[test]
fn a_reused_frame_starts_with_void_locals_after_heap_and_scalar_releases() {
    // main calls two functions with one local, then asks a third function with
    // the same shape to read its untouched local. The first release drops a
    // string; the second deliberately omits a scalar from its plan. Neither
    // value may become visible through the reused frame.
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![
            function(
                "main",
                0,
                vec![
                    I::Call(1),
                    I::Pop,
                    I::Call(2),
                    I::Pop,
                    I::Call(3),
                    I::Return,
                ],
                FrameRelease::EveryLocal,
            ),
            function(
                "heap_seed",
                1,
                vec![I::ConstStr(0), I::StoreLocal(0), I::ReturnVoid],
                FrameRelease::EveryLocal,
            ),
            function(
                "scalar_seed",
                1,
                vec![I::ConstInt(42), I::StoreLocal(0), I::ReturnVoid],
                FrameRelease::Planned(Vec::new()),
            ),
            function(
                "observe",
                1,
                vec![I::LoadLocal(0), I::Return],
                FrameRelease::EveryLocal,
            ),
        ],
        main: Some(0),
        strings: vec!["released".to_owned()],
    };
    let mut host = CapturingHost::new();
    let outcome = execute(&module, &mut host).expect("clean run");
    assert_eq!(outcome.result, Value::Void);
    assert_eq!(outcome.heap.current, 0);
}
