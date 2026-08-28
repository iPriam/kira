//! Small repeatable VM dispatch workload.
//!
//! Run with:
//!
//! ```text
//! cargo run -p kira-vm-runtime --example dispatch_benchmark --release
//! ```
//!
//! The loop keeps its values scalar so the result mostly measures instruction
//! dispatch rather than heap allocation or host I/O.

use std::time::Instant;

use kira_bytecode::FrameRelease;
use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction;
use kira_runtime_abi::CapturingHost;
use kira_vm_runtime::execute;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let iterations = 10_000_000i64;
    let calls = std::env::args().nth(1).as_deref() == Some("calls");
    let module = if calls {
        call_module(iterations)
    } else {
        loop_module(iterations)
    };
    let mut host = CapturingHost::new();
    let started = Instant::now();
    let outcome = execute(&module, &mut host)?;
    let elapsed = started.elapsed();
    println!(
        "{iterations} {} iterations: {elapsed:?} ({:.2} ns/iteration)",
        if calls { "call" } else { "scalar loop" },
        elapsed.as_secs_f64() * 1_000_000_000.0 / iterations as f64,
    );
    if outcome.heap.current != 0 {
        return Err(format!("benchmark leaked {} heap objects", outcome.heap.current).into());
    }
    Ok(())
}

fn loop_module(iterations: i64) -> Module {
    // local 0 is the counter. The loop is:
    //
    //   counter += 1;
    //   if counter < iterations { continue }
    //   return;
    //
    // Keeping the branch targets explicit makes this fixture independent of
    // the compiler and makes validation exercise the same jump checks as real
    // bytecode.
    let code = vec![
        Instruction::ConstInt(0),
        Instruction::StoreLocal(0),
        Instruction::LoadLocal(0),
        Instruction::ConstInt(1),
        Instruction::AddInt,
        Instruction::StoreLocal(0),
        Instruction::LoadLocal(0),
        Instruction::ConstInt(iterations),
        Instruction::LtInt,
        Instruction::JumpIfFalse(11),
        Instruction::Jump(2),
        Instruction::ReturnVoid,
    ];
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        functions: vec![FuncProto {
            name: "benchmark".to_owned(),
            param_count: 0,
            local_count: 1,
            execution: kira_runtime_abi::Execution::Runtime,
            code,
            releases: FrameRelease::EveryLocal,
        }],
        main: Some(0),
        strings: Vec::new(),
    }
}

/// Builds the same loop with one bytecode call per iteration.
///
/// This mode exercises the frame cache: `tick` returns before the next
/// iteration enters it, so its local vector can be reused instead of being
/// allocated for every call.
fn call_module(iterations: i64) -> Module {
    let main = FuncProto {
        name: "benchmark".to_owned(),
        param_count: 0,
        local_count: 1,
        execution: kira_runtime_abi::Execution::Runtime,
        code: vec![
            Instruction::ConstInt(0),
            Instruction::StoreLocal(0),
            Instruction::LoadLocal(0),
            Instruction::Call(1),
            Instruction::StoreLocal(0),
            Instruction::LoadLocal(0),
            Instruction::ConstInt(iterations),
            Instruction::LtInt,
            Instruction::JumpIfFalse(10),
            Instruction::Jump(2),
            Instruction::ReturnVoid,
        ],
        releases: FrameRelease::EveryLocal,
    };
    let tick = FuncProto {
        name: "tick".to_owned(),
        param_count: 1,
        local_count: 1,
        execution: kira_runtime_abi::Execution::Runtime,
        code: vec![
            Instruction::LoadLocal(0),
            Instruction::ConstInt(1),
            Instruction::AddInt,
            Instruction::Return,
        ],
        releases: FrameRelease::EveryLocal,
    };
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
        constants: Vec::new(),
        functions: vec![main, tick],
        main: Some(0),
        strings: Vec::new(),
    }
}
