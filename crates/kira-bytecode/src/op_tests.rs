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
fn round_trips_the_unsigned_arithmetic_and_ordering_opcodes() {
    // The six opcodes the `U8`..`U64` spellings need. Round-tripping them
    // separately keeps a decoder that silently mapped one onto its signed twin
    // from passing: `DivUInt` must come back as `DivUInt`, not `DivInt`.
    let code = vec![
        Instruction::DivUInt,
        Instruction::RemUInt,
        Instruction::LtUInt,
        Instruction::LeUInt,
        Instruction::GtUInt,
        Instruction::GeUInt,
    ];
    let bytes = encode(&code);
    assert_eq!(decode(&bytes).unwrap(), code);
}

#[test]
fn the_unsigned_opcodes_are_appended_after_the_previous_last_one() {
    // Opcodes are append-only: a module already written decodes by these
    // numbers, so the unsigned set starts one past `ENUM_PAYLOAD` and nothing
    // before it moved. Spelled literally so a renumber fails here rather than
    // silently redirecting an existing artifact.
    assert_eq!(opcode::ENUM_PAYLOAD, 0x37);
    assert_eq!(opcode::DIV_UINT, 0x38);
    assert_eq!(opcode::REM_UINT, 0x39);
    assert_eq!(opcode::LT_UINT, 0x3a);
    assert_eq!(opcode::LE_UINT, 0x3b);
    assert_eq!(opcode::GT_UINT, 0x3c);
    assert_eq!(opcode::GE_UINT, 0x3d);
    // The signed opcodes they sit beside are untouched.
    assert_eq!(opcode::DIV_INT, 0x0f);
    assert_eq!(opcode::REM_INT, 0x10);
    assert_eq!(opcode::LT_INT, 0x18);
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
