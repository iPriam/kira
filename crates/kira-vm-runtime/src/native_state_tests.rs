//! VM execution tests for opaque callback-state instructions.

use kira_bytecode::module::{FuncProto, Module};
use kira_bytecode::op::{FieldPath, Instruction as I};
use kira_runtime_abi::{
    CapturingHost, Execution, NativeStateError, NativeStateHost, NativeStateTypeId,
};

use crate::{VmError, execute};

const STATE_TYPE: NativeStateTypeId = NativeStateTypeId::new(0x0500_0000_0000_0000);

fn module(code: Vec<I>, locals: u16) -> Module {
    Module {
        exports: Default::default(),
        foreign_imports: Vec::new(),
        foreign_aggregates: Default::default(),
        functions: vec![FuncProto {
            name: "main".to_owned(),
            param_count: 0,
            local_count: locals,
            execution: Execution::Runtime,
            code,
        }],
        main: Some(0),
        strings: Vec::new(),
    }
}

#[test]
fn boxes_recovers_mutates_observes_and_frees_state() {
    let field_zero = FieldPath::new(vec![0]).expect("one field");
    let module = module(
        vec![
            I::ConstInt(0),
            I::ConstInt(0),
            I::NewStruct(2),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeUserData,
            I::StoreLocal(1),
            I::LoadLocal(1),
            I::NativeRecover(STATE_TYPE.as_word()),
            I::StoreLocal(2),
            I::LoadLocal(2),
            I::GetField(0),
            I::ConstInt(1),
            I::AddInt,
            I::StoreField {
                slot: 2,
                path: field_zero,
            },
            I::LoadLocal(2),
            I::GetField(0),
            I::Print,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::ReturnVoid,
        ],
        3,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    let outcome = execute(&module, &mut host).expect("state flow executes");
    assert_eq!(host.inner().lines(), ["1"]);
    assert_eq!(outcome.heap.current, 0);
}

#[test]
fn wrong_recovery_type_and_double_free_are_typed_traps() {
    let wrong = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::NativeUserData,
            I::NativeRecover(STATE_TYPE.as_word() + 1),
            I::ReturnVoid,
        ],
        0,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    assert!(matches!(
        execute(&wrong, &mut host),
        Err(VmError::NativeState(NativeStateError::WrongType { .. }))
    ));

    let double_free = module(
        vec![
            I::ConstInt(0),
            I::NewStruct(1),
            I::NativeState(STATE_TYPE.as_word()),
            I::StoreLocal(0),
            I::LoadLocal(0),
            I::NativeStateFree,
            I::Pop,
            I::LoadLocal(0),
            I::NativeStateFree,
            I::ReturnVoid,
        ],
        1,
    );
    let mut host = NativeStateHost::new(CapturingHost::new());
    assert!(matches!(
        execute(&double_free, &mut host),
        Err(VmError::NativeState(NativeStateError::UnknownToken(_)))
    ));
}
