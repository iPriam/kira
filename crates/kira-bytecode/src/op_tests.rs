//! Encode/decode round-trip and rejection tests for the instruction set,
//! split out of `op.rs` on the file-size ladder.

use super::*;

#[test]
fn round_trips_a_mixed_instruction_stream() {
    let code = vec![
        Instruction::ConstInt(-42),
        Instruction::ConstFloat(3.5),
        Instruction::ConstBool(true),
        Instruction::ConstStr(7),
        Instruction::LoadLocal(3),
        Instruction::StoreLocal(9),
        Instruction::AddInt,
        Instruction::ConcatStr,
        Instruction::JumpIfFalse(12),
        Instruction::Jump(0),
        Instruction::Call(2),
        Instruction::NewEnum {
            tag: 3,
            has_payload: true,
        },
        Instruction::NewEnum {
            tag: 0,
            has_payload: false,
        },
        Instruction::EnumTag,
        Instruction::EnumPayload,
        Instruction::Print,
        Instruction::ReturnVoid,
        Instruction::Return,
    ];
    let bytes = encode(&code);
    assert_eq!(decode(&bytes).unwrap(), code);
}

#[test]
fn unknown_opcode_is_reported() {
    let err = decode(&[0xff]).unwrap_err();
    assert!(matches!(
        err,
        DecodeError::UnknownOpcode { opcode: 0xff, .. }
    ));
}

#[test]
fn truncated_operand_is_reported() {
    // CONST_INT opcode with no following 8-byte payload.
    let err = decode(&[0x01, 0x00]).unwrap_err();
    assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
}
