use super::*;

#[test]
fn the_main_thread_opcodes_are_appended_after_load_constant() {
    assert_eq!(opcode::LOAD_CONSTANT, 0x73);
    assert_eq!(opcode::MAIN_THREAD_CALL, 0x74);
    assert_eq!(opcode::MAIN_THREAD_JOIN, 0x75);
    assert_eq!(opcode::MAIN_THREAD_LIFECYCLE, 0x76);
}

#[test]
fn round_trips_main_thread_operations() {
    let code = vec![
        Instruction::MainThreadCall {
            operation: MainThreadOp::Invoke,
            function: 4,
            args: 2,
        },
        Instruction::MainThreadCall {
            operation: MainThreadOp::Spawn,
            function: u64::MAX,
            args: 0,
        },
        Instruction::MainThreadCall {
            operation: MainThreadOp::Post,
            function: 8,
            args: 1,
        },
        Instruction::MainThreadJoin,
        Instruction::MainThreadLifecycle,
    ];
    assert_eq!(decode(&encode(&code)).unwrap(), code);
}

#[test]
fn a_main_thread_operation_with_an_unknown_kind_is_rejected() {
    let bytes = [
        opcode::MAIN_THREAD_CALL,
        0xfe,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
    ];
    let err = decode(&bytes).unwrap_err();
    assert!(matches!(
        err,
        DecodeError::UnknownOpcode { opcode: 0xfe, .. }
    ));
}
