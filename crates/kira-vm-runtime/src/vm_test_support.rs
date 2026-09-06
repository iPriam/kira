//! Fixtures every VM test module builds on: run a module to completion, and
//! declare a function without repeating the struct literal.

use super::*;
use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::Instruction as I;
use kira_runtime_abi::CapturingHost;

pub(crate) fn run(module: &Module) -> (Vec<String>, RunOutcome) {
    let mut host = CapturingHost::new();
    let outcome = execute(module, &mut host).expect("clean run");
    (host.lines().to_vec(), outcome)
}

pub(crate) fn func(name: &str, params: u64, locals: u64, code: Vec<I>) -> FuncProto {
    FuncProto {
        name: name.to_owned(),
        param_count: params,
        local_count: locals,
        execution: kira_runtime_abi::Execution::Runtime,
        code,
        releases: kira_bytecode::FrameRelease::EveryLocal,
    }
}
