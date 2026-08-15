//! VM execution at the bytecode format's former operand boundaries.

use super::{Value, execute};
use kira_bytecode::module::{FrameRelease, FuncProto, Module};
use kira_bytecode::op::Instruction;
use kira_runtime_abi::{CapturingHost, Execution};

fn module(local_count: u64, code: Vec<Instruction>) -> Module {
    Module {
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count,
            execution: Execution::Runtime,
            code,
            releases: FrameRelease::EveryLocal,
        }],
        main: Some(0),
        strings: Vec::new(),
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        foreign_callbacks: Vec::new(),
    }
}

#[test]
fn vm_executes_a_local_above_the_legacy_slot_limit() {
    let slot = u64::from(u16::MAX) + 1;
    let mut host = CapturingHost::new();
    let outcome = execute(
        &module(
            slot + 1,
            vec![
                Instruction::ConstInt(42),
                Instruction::StoreLocal(slot),
                Instruction::LoadLocal(slot),
                Instruction::Return,
            ],
        ),
        &mut host,
    )
    .expect("the VM accepts a wide local slot");

    assert_eq!(outcome.result, Value::Int(42));
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn vm_executes_an_enum_tag_above_the_legacy_variant_limit() {
    let tag = u64::from(u16::MAX) + 1;
    let mut host = CapturingHost::new();
    let outcome = execute(
        &module(
            0,
            vec![
                Instruction::NewEnum {
                    tag,
                    has_payload: false,
                },
                Instruction::EnumTag,
                Instruction::Return,
            ],
        ),
        &mut host,
    )
    .expect("the VM accepts a wide enum tag");

    assert_eq!(outcome.result, Value::Int(tag as i64));
    assert_eq!(outcome.heap.current, 0);
}
