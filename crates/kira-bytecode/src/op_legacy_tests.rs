//! KBC1 instruction-width compatibility tests.

use super::*;

#[test]
fn legacy_codec_reads_the_old_width_for_each_bytecode_owned_operand() {
    let mut bytes = Vec::new();
    let push_u16 = |bytes: &mut Vec<u8>, value: u16| {
        bytes.extend_from_slice(&value.to_le_bytes());
    };
    let push_u32 = |bytes: &mut Vec<u8>, value: u32| {
        bytes.extend_from_slice(&value.to_le_bytes());
    };

    bytes.extend_from_slice(&[opcode::CONST_STR]);
    push_u32(&mut bytes, 7);
    bytes.extend_from_slice(&[opcode::LOAD_LOCAL]);
    push_u16(&mut bytes, 9);
    bytes.extend_from_slice(&[opcode::STORE_LOCAL]);
    push_u16(&mut bytes, 10);
    bytes.extend_from_slice(&[opcode::JUMP]);
    push_u32(&mut bytes, 11);
    bytes.extend_from_slice(&[opcode::JUMP_IF_FALSE]);
    push_u32(&mut bytes, 12);
    bytes.extend_from_slice(&[opcode::CALL]);
    push_u32(&mut bytes, 13);
    bytes.extend_from_slice(&[opcode::CALL_MUT]);
    push_u32(&mut bytes, 14);
    push_u16(&mut bytes, 15);
    push_u16(&mut bytes, 2);
    bytes.extend_from_slice(&[step_tag::FIELD]);
    push_u16(&mut bytes, 16);
    bytes.push(step_tag::INDEX);
    bytes.extend_from_slice(&[opcode::CALL_WRITEBACK]);
    push_u32(&mut bytes, 17);
    push_u16(&mut bytes, 1);
    push_u16(&mut bytes, 18);
    push_u16(&mut bytes, 19);
    push_u16(&mut bytes, 0);
    bytes.extend_from_slice(&[opcode::CALL_NATIVE_WRITEBACK]);
    push_u32(&mut bytes, 20);
    push_u16(&mut bytes, 1);
    push_u16(&mut bytes, 21);
    push_u16(&mut bytes, 22);
    push_u16(&mut bytes, 0);
    bytes.extend_from_slice(&[opcode::NEW_STRUCT]);
    push_u16(&mut bytes, 23);
    bytes.extend_from_slice(&[opcode::GET_FIELD]);
    push_u16(&mut bytes, 24);
    bytes.extend_from_slice(&[opcode::STORE_FIELD]);
    push_u16(&mut bytes, 25);
    push_u16(&mut bytes, 2);
    push_u16(&mut bytes, 26);
    push_u16(&mut bytes, 27);
    bytes.extend_from_slice(&[opcode::NEW_ARRAY]);
    push_u32(&mut bytes, 28);
    bytes.extend_from_slice(&[opcode::ARRAY_GET_LOCAL]);
    push_u16(&mut bytes, 29);
    bytes.extend_from_slice(&[opcode::NEW_ENUM]);
    push_u16(&mut bytes, 30);
    bytes.push(1);
    bytes.extend_from_slice(&[opcode::CELL_GET]);
    push_u16(&mut bytes, 31);
    bytes.extend_from_slice(&[opcode::CELL_SET]);
    push_u16(&mut bytes, 32);
    bytes.push(opcode::RETURN_VOID);

    let expected = vec![
        Instruction::ConstStr(7),
        Instruction::LoadLocal(9),
        Instruction::StoreLocal(10),
        Instruction::Jump(11),
        Instruction::JumpIfFalse(12),
        Instruction::Call(13),
        Instruction::CallMut {
            func: 14,
            slot: 15,
            path: PlacePath::new(vec![PathStep::Field(16), PathStep::Index]),
        },
        Instruction::CallWriteback {
            func: 17,
            targets: vec![WritebackTarget {
                param: 18,
                slot: 19,
                path: PlacePath::new(Vec::new()),
            }],
        },
        Instruction::CallNativeWriteback {
            func: 20,
            targets: vec![WritebackTarget {
                param: 21,
                slot: 22,
                path: PlacePath::new(Vec::new()),
            }],
        },
        Instruction::NewStruct(23),
        Instruction::GetField(24),
        Instruction::StoreField {
            slot: 25,
            path: FieldPath::new(vec![26, 27]),
        },
        Instruction::NewArray(28),
        Instruction::ArrayGetLocal(29),
        Instruction::NewEnum {
            tag: 30,
            has_payload: true,
        },
        Instruction::CellGet(31),
        Instruction::CellSet(32),
        Instruction::ReturnVoid,
    ];
    assert_eq!(decode_legacy(&bytes).unwrap(), expected);
}
