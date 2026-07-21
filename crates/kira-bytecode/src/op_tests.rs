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
fn round_trips_the_bitwise_and_shift_opcodes() {
    // As with the unsigned set, round-tripping these together keeps a decoder
    // that collapsed one onto another from passing: `ShrUInt` must come back as
    // `ShrUInt`, never as `ShrInt`, because the two disagree on every negative
    // input.
    let code = vec![
        Instruction::BitAnd,
        Instruction::BitOr,
        Instruction::BitXor,
        Instruction::Shl,
        Instruction::ShrInt,
        Instruction::ShrUInt,
        Instruction::BitNot,
    ];
    let bytes = encode(&code);
    assert_eq!(decode(&bytes).unwrap(), code);
}

#[test]
fn the_bitwise_opcodes_are_appended_after_the_unsigned_ones() {
    // Append-only, again spelled literally: the bitwise set starts one past
    // `GE_UINT`, which was the last opcode before it, and nothing earlier
    // moved. A renumber fails here rather than silently redirecting a module
    // that is already on disk.
    assert_eq!(opcode::GE_UINT, 0x3d);
    assert_eq!(opcode::BIT_AND, 0x3e);
    assert_eq!(opcode::BIT_OR, 0x3f);
    assert_eq!(opcode::BIT_XOR, 0x40);
    assert_eq!(opcode::SHL, 0x41);
    assert_eq!(opcode::SHR_INT, 0x42);
    assert_eq!(opcode::SHR_UINT, 0x43);
    assert_eq!(opcode::BIT_NOT, 0x44);
}

#[test]
fn the_foreign_call_opcode_is_appended_after_bit_not() {
    // `CALL_FOREIGN` is the first opcode after `BIT_NOT`, appended so that a
    // module already on disk keeps its meaning. A renumber fails here.
    assert_eq!(opcode::BIT_NOT, 0x44);
    assert_eq!(opcode::CALL_FOREIGN, 0x45);
}

#[test]
fn round_trips_a_foreign_call() {
    let code = vec![
        Instruction::CallForeign(0),
        Instruction::CallForeign(4_294_967_295),
        Instruction::ReturnVoid,
    ];
    let bytes = encode(&code);
    assert_eq!(decode(&bytes).unwrap(), code);
}

#[test]
fn a_truncated_foreign_call_operand_is_reported() {
    // CALL_FOREIGN opcode with fewer than four operand bytes.
    let err = decode(&[opcode::CALL_FOREIGN, 0x00, 0x00]).unwrap_err();
    assert!(matches!(err, DecodeError::UnexpectedEnd { .. }));
}

#[test]
fn round_trips_the_scalar_conversion_opcodes() {
    // The two cross-representation numeric conversions. Round-tripping them
    // together keeps a decoder that swapped the two directions from passing:
    // `ConvertFloatToInt` must never come back as `ConvertIntToFloat`, because
    // they disagree on every input.
    let code = vec![
        Instruction::ConvertIntToFloat,
        Instruction::ConvertFloatToInt,
    ];
    let bytes = encode(&code);
    assert_eq!(decode(&bytes).unwrap(), code);
}

#[test]
fn the_conversion_opcodes_are_appended_after_the_foreign_call() {
    // Append-only, spelled literally: the conversion opcodes start one past
    // `CALL_FOREIGN`, which was the last opcode before them, and nothing
    // earlier moved. A renumber fails here rather than silently redirecting a
    // module already on disk.
    assert_eq!(opcode::CALL_FOREIGN, 0x45);
    assert_eq!(opcode::CONVERT_INT_TO_FLOAT, 0x46);
    assert_eq!(opcode::CONVERT_FLOAT_TO_INT, 0x47);
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
