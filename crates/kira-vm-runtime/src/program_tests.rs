//! Whole programs the VM runs end to end, and the modules it must reject
//! rather than panic on.

use super::*;
use crate::vm_test_support::{func, run};
use kira_bytecode::module::Module;
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

#[test]
fn arithmetic_and_print() {
    // print(17 % 5); print(-4); print(20 / 4 / 2)
    let main = func(
        "main",
        0,
        0,
        vec![
            I::ConstInt(17),
            I::ConstInt(5),
            I::RemInt,
            I::Print,
            I::Pop,
            I::ConstInt(4),
            I::NegInt,
            I::Print,
            I::Pop,
            I::ConstInt(20),
            I::ConstInt(4),
            I::DivInt,
            I::ConstInt(2),
            I::DivInt,
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec![],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["2", "-4", "2"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn while_loop_accumulates() {
    // var i = 0; var sum = 0; while i < 5 { sum = sum + i; i = i + 1 } print(sum)
    // locals: 0 = i, 1 = sum
    let main = func(
        "main",
        0,
        2,
        vec![
            I::ConstInt(0),
            I::StoreLocal(0),
            I::ConstInt(0),
            I::StoreLocal(1),
            // loop start (index 4)
            I::LoadLocal(0),
            I::ConstInt(5),
            I::LtInt,
            I::JumpIfFalse(17),
            I::LoadLocal(1),
            I::LoadLocal(0),
            I::AddInt,
            I::StoreLocal(1),
            I::LoadLocal(0),
            I::ConstInt(1),
            I::AddInt,
            I::StoreLocal(0),
            I::Jump(4),
            I::LoadLocal(1), // index 17: loop exit
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec![],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["10"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn signed_integer_comparisons_keep_their_ordering() {
    let main = func(
        "main",
        0,
        0,
        vec![
            I::ConstInt(4),
            I::ConstInt(4),
            I::EqInt,
            I::Print,
            I::Pop,
            I::ConstInt(4),
            I::ConstInt(4),
            I::NeInt,
            I::Print,
            I::Pop,
            I::ConstInt(3),
            I::ConstInt(4),
            I::LeInt,
            I::Print,
            I::Pop,
            I::ConstInt(5),
            I::ConstInt(4),
            I::GtInt,
            I::Print,
            I::Pop,
            I::ConstInt(4),
            I::ConstInt(4),
            I::GeInt,
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec![],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["true", "false", "true", "true", "true"]);
    assert_eq!(outcome.heap.current, 0);
}

/// Builds a two-function module computing and printing `fib(n)`.
fn fib_module(n: i64) -> Module {
    // main (index 0): print(fib(n))
    let main = func(
        "main",
        0,
        0,
        vec![I::ConstInt(n), I::Call(1), I::Print, I::Pop, I::ReturnVoid],
    );
    // fib (index 1): if n < 2 { return n } return fib(n-1) + fib(n-2)
    let fib = func(
        "fib",
        1,
        1,
        vec![
            I::LoadLocal(0),
            I::ConstInt(2),
            I::LtInt,
            I::JumpIfFalse(6),
            I::LoadLocal(0),
            I::Return,
            I::LoadLocal(0), // index 6
            I::ConstInt(1),
            I::SubInt,
            I::Call(1),
            I::LoadLocal(0),
            I::ConstInt(2),
            I::SubInt,
            I::Call(1),
            I::AddInt,
            I::Return,
        ],
    );
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main, fib],
        main: Some(0),
        strings: vec![],
    }
}

#[test]
fn recursion_computes_fib_20() {
    let (lines, outcome) = run(&fib_module(20));
    assert_eq!(lines, ["6765"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn strings_concatenate_and_fully_reclaim() {
    // let s = "foo"; let t = "bar"; print(s + t)  — locals 0 = s, 1 = t
    let main = func(
        "main",
        0,
        2,
        vec![
            I::ConstStr(0),
            I::StoreLocal(0),
            I::ConstStr(1),
            I::StoreLocal(1),
            I::LoadLocal(0),
            I::LoadLocal(1),
            I::ConcatStr,
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec!["foo".to_owned(), "bar".to_owned()],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["foobar"]);
    // Every string (two literals, two clones on read, one concat result)
    // is reclaimed by exit.
    assert_eq!(outcome.heap.current, 0);
    assert!(outcome.heap.allocated >= 3);
    assert_eq!(outcome.heap.allocated, outcome.heap.freed);
}

#[test]
fn malformed_modules_are_rejected_typed_not_panicking() {
    // Each of these once panicked the interpreter; validation now rejects
    // them with a typed VmError before execution starts.
    let malformed = vec![
        // Empty code.
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 0, 0, vec![])],
            main: Some(0),
            strings: vec![],
        },
        // Falls off the end (not return-terminated).
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 0, 0, vec![I::ConstInt(1)])],
            main: Some(0),
            strings: vec![],
        },
        // ConstStr into an empty pool.
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 0, 0, vec![I::ConstStr(3), I::ReturnVoid])],
            main: Some(0),
            strings: vec![],
        },
        // LoadLocal beyond local_count.
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 0, 1, vec![I::LoadLocal(9), I::ReturnVoid])],
            main: Some(0),
            strings: vec![],
        },
        // More parameters than locals (fill_params would underflow slots).
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 2, 0, vec![I::ReturnVoid])],
            main: Some(0),
            strings: vec![],
        },
        // Entrypoint out of range.
        Module {
            exports: Default::default(),
            foreign_imports: Vec::new(),
            foreign_aggregates: Default::default(),
            foreign_callbacks: Vec::new(),
            constants: Vec::new(),
            types: Vec::new(),
            functions: vec![func("main", 0, 0, vec![I::ReturnVoid])],
            main: Some(7),
            strings: vec![],
        },
    ];
    for module in malformed {
        let mut host = CapturingHost::new();
        let error = execute(&module, &mut host).unwrap_err();
        assert!(
            matches!(error, VmError::Module(_)),
            "expected typed module rejection, got {error:?}"
        );
    }
}

#[test]
fn string_local_reassignment_frees_the_old_value() {
    // var s = "a"; s = "bb"; print(s)  — local 0 = s
    let main = func(
        "main",
        0,
        1,
        vec![
            I::ConstStr(0),
            I::StoreLocal(0),
            I::ConstStr(1),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::Print,
            I::Pop,
            I::ReturnVoid,
        ],
    );
    let module = Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        types: Vec::new(),
        functions: vec![main],
        main: Some(0),
        strings: vec!["a".to_owned(), "bb".to_owned()],
    };
    let (lines, outcome) = run(&module);
    assert_eq!(lines, ["bb"]);
    assert_eq!(outcome.heap.current, 0);
}
