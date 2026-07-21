//! The byte codec: [`Instruction`]s to bytes and back.
//!
//! Split from the instruction model, which owns the [`Instruction`] enum, the
//! place-path types, and the opcode constants. Those are definitions; this is
//! the only code that turns them into bytes, so the append-only contract has
//! exactly one place to be honored on each side.
//!
//! Decoding validates rather than trusts. A `Module` is a public,
//! deserializable artifact, so every truncation and every unknown byte returns
//! a typed [`DecodeError`] — nothing here panics on malformed input, and an
//! unknown opcode is rejected rather than guessed at.

use super::{FieldPath, Instruction, PathStep, PlacePath, opcode as o, step_tag};

/// An error decoding a byte stream back into instructions.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DecodeError {
    /// The byte stream ended in the middle of an instruction.
    #[error("unexpected end of bytecode at offset {offset}")]
    UnexpectedEnd {
        /// Byte offset where decoding ran out of input.
        offset: usize,
    },
    /// An opcode byte does not name any instruction.
    #[error("unknown opcode {opcode:#04x} at offset {offset}")]
    UnknownOpcode {
        /// The unrecognized opcode byte.
        opcode: u8,
        /// Byte offset of the opcode.
        offset: usize,
    },
}

/// Appends the byte encoding of one instruction to `out`.
pub fn encode_one(instruction: &Instruction, out: &mut Vec<u8>) {
    match instruction {
        Instruction::ConstInt(value) => {
            out.push(o::CONST_INT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Instruction::ConstFloat(value) => {
            out.push(o::CONST_FLOAT);
            out.extend_from_slice(&value.to_le_bytes());
        }
        Instruction::ConstBool(value) => {
            out.push(o::CONST_BOOL);
            out.push(u8::from(*value));
        }
        Instruction::ConstStr(index) => {
            out.push(o::CONST_STR);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Instruction::LoadLocal(slot) => {
            out.push(o::LOAD_LOCAL);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::StoreLocal(slot) => {
            out.push(o::STORE_LOCAL);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::Jump(target) => {
            out.push(o::JUMP);
            out.extend_from_slice(&target.to_le_bytes());
        }
        Instruction::JumpIfFalse(target) => {
            out.push(o::JUMP_IF_FALSE);
            out.extend_from_slice(&target.to_le_bytes());
        }
        Instruction::Call(index) => {
            out.push(o::CALL);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Instruction::CallNative(id) => {
            out.push(o::CALL_NATIVE);
            out.extend_from_slice(&id.to_le_bytes());
        }
        Instruction::CallForeign(id) => {
            out.push(o::CALL_FOREIGN);
            out.extend_from_slice(&id.to_le_bytes());
        }
        Instruction::CallMut { func, slot, path } => {
            out.push(o::CALL_MUT);
            out.extend_from_slice(&func.to_le_bytes());
            encode_place(*slot, path, out);
        }
        Instruction::NewStruct(fields) => {
            out.push(o::NEW_STRUCT);
            out.extend_from_slice(&fields.to_le_bytes());
        }
        Instruction::GetField(index) => {
            out.push(o::GET_FIELD);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Instruction::StoreField { slot, path } => {
            out.push(o::STORE_FIELD);
            out.extend_from_slice(&slot.to_le_bytes());
            out.extend_from_slice(&path.len().to_le_bytes());
            for step in path.steps() {
                out.extend_from_slice(&step.to_le_bytes());
            }
        }
        Instruction::NewArray(count) => {
            out.push(o::NEW_ARRAY);
            out.extend_from_slice(&count.to_le_bytes());
        }
        Instruction::StorePlace { slot, path } => {
            out.push(o::STORE_PLACE);
            encode_place(*slot, path, out);
        }
        Instruction::ArrayAppend { slot, path } => {
            out.push(o::ARRAY_APPEND);
            encode_place(*slot, path, out);
        }
        Instruction::NewEnum { tag, has_payload } => {
            out.push(o::NEW_ENUM);
            out.extend_from_slice(&tag.to_le_bytes());
            out.push(u8::from(*has_payload));
        }
        // Nullary instructions: one exhaustive arm each, so encoding is total
        // by construction (no fallthrough, no panic path).
        Instruction::ArrayGet => out.push(o::ARRAY_GET),
        Instruction::ArrayLen => out.push(o::ARRAY_LEN),
        Instruction::EnumTag => out.push(o::ENUM_TAG),
        Instruction::EnumPayload => out.push(o::ENUM_PAYLOAD),
        Instruction::ConvertIntToFloat => out.push(o::CONVERT_INT_TO_FLOAT),
        Instruction::ConvertFloatToInt => out.push(o::CONVERT_FLOAT_TO_INT),
        Instruction::ConstVoid => out.push(o::CONST_VOID),
        Instruction::Pop => out.push(o::POP),
        Instruction::NegInt => out.push(o::NEG_INT),
        Instruction::NegFloat => out.push(o::NEG_FLOAT),
        Instruction::Not => out.push(o::NOT),
        Instruction::AddInt => out.push(o::ADD_INT),
        Instruction::SubInt => out.push(o::SUB_INT),
        Instruction::MulInt => out.push(o::MUL_INT),
        Instruction::DivInt => out.push(o::DIV_INT),
        Instruction::RemInt => out.push(o::REM_INT),
        Instruction::DivUInt => out.push(o::DIV_UINT),
        Instruction::RemUInt => out.push(o::REM_UINT),
        Instruction::AddFloat => out.push(o::ADD_FLOAT),
        Instruction::SubFloat => out.push(o::SUB_FLOAT),
        Instruction::MulFloat => out.push(o::MUL_FLOAT),
        Instruction::DivFloat => out.push(o::DIV_FLOAT),
        Instruction::ConcatStr => out.push(o::CONCAT_STR),
        Instruction::EqInt => out.push(o::EQ_INT),
        Instruction::NeInt => out.push(o::NE_INT),
        Instruction::LtInt => out.push(o::LT_INT),
        Instruction::LeInt => out.push(o::LE_INT),
        Instruction::GtInt => out.push(o::GT_INT),
        Instruction::GeInt => out.push(o::GE_INT),
        Instruction::LtUInt => out.push(o::LT_UINT),
        Instruction::LeUInt => out.push(o::LE_UINT),
        Instruction::GtUInt => out.push(o::GT_UINT),
        Instruction::GeUInt => out.push(o::GE_UINT),
        Instruction::BitAnd => out.push(o::BIT_AND),
        Instruction::BitOr => out.push(o::BIT_OR),
        Instruction::BitXor => out.push(o::BIT_XOR),
        Instruction::Shl => out.push(o::SHL),
        Instruction::ShrInt => out.push(o::SHR_INT),
        Instruction::ShrUInt => out.push(o::SHR_UINT),
        Instruction::BitNot => out.push(o::BIT_NOT),
        Instruction::EqFloat => out.push(o::EQ_FLOAT),
        Instruction::NeFloat => out.push(o::NE_FLOAT),
        Instruction::LtFloat => out.push(o::LT_FLOAT),
        Instruction::LeFloat => out.push(o::LE_FLOAT),
        Instruction::GtFloat => out.push(o::GT_FLOAT),
        Instruction::GeFloat => out.push(o::GE_FLOAT),
        Instruction::EqBool => out.push(o::EQ_BOOL),
        Instruction::NeBool => out.push(o::NE_BOOL),
        Instruction::EqStr => out.push(o::EQ_STR),
        Instruction::NeStr => out.push(o::NE_STR),
        Instruction::Print => out.push(o::PRINT),
        Instruction::Return => out.push(o::RETURN),
        Instruction::ReturnVoid => out.push(o::RETURN_VOID),
    }
}

/// Appends a place operand — slot, step count, then one tagged step each.
fn encode_place(slot: u16, path: &PlacePath, out: &mut Vec<u8>) {
    out.extend_from_slice(&slot.to_le_bytes());
    out.extend_from_slice(&path.len().to_le_bytes());
    for step in path.steps() {
        match step {
            PathStep::Field(index) => {
                out.push(step_tag::FIELD);
                out.extend_from_slice(&index.to_le_bytes());
            }
            // An index carries no immediate: its value is on the stack.
            PathStep::Index => out.push(step_tag::INDEX),
        }
    }
}

/// Encodes a whole instruction sequence to bytes.
pub fn encode(code: &[Instruction]) -> Vec<u8> {
    let mut out = Vec::with_capacity(code.len());
    for instruction in code {
        encode_one(instruction, &mut out);
    }
    out
}

/// Decodes a byte stream back into an instruction sequence.
pub fn decode(bytes: &[u8]) -> Result<Vec<Instruction>, DecodeError> {
    let mut cursor = Cursor { bytes, offset: 0 };
    let mut code = Vec::new();
    while cursor.offset < bytes.len() {
        code.push(cursor.next_instruction()?);
    }
    Ok(code)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let end = self.offset + N;
        let slice = self
            .bytes
            .get(self.offset..end)
            .ok_or(DecodeError::UnexpectedEnd {
                offset: self.offset,
            })?;
        let mut array = [0u8; N];
        array.copy_from_slice(slice);
        self.offset = end;
        Ok(array)
    }

    fn next_instruction(&mut self) -> Result<Instruction, DecodeError> {
        let opcode_offset = self.offset;
        let [op] = self.take::<1>()?;
        let instruction = match op {
            o::CONST_INT => Instruction::ConstInt(i64::from_le_bytes(self.take()?)),
            o::CONST_FLOAT => Instruction::ConstFloat(f64::from_le_bytes(self.take()?)),
            o::CONST_BOOL => Instruction::ConstBool(self.take::<1>()?[0] != 0),
            o::CONST_STR => Instruction::ConstStr(u32::from_le_bytes(self.take()?)),
            o::CONST_VOID => Instruction::ConstVoid,
            o::LOAD_LOCAL => Instruction::LoadLocal(u16::from_le_bytes(self.take()?)),
            o::STORE_LOCAL => Instruction::StoreLocal(u16::from_le_bytes(self.take()?)),
            o::JUMP => Instruction::Jump(u32::from_le_bytes(self.take()?)),
            o::JUMP_IF_FALSE => Instruction::JumpIfFalse(u32::from_le_bytes(self.take()?)),
            o::CALL => Instruction::Call(u32::from_le_bytes(self.take()?)),
            o::CALL_NATIVE => Instruction::CallNative(u32::from_le_bytes(self.take()?)),
            o::CALL_FOREIGN => Instruction::CallForeign(u32::from_le_bytes(self.take()?)),
            o::NEW_STRUCT => Instruction::NewStruct(u16::from_le_bytes(self.take()?)),
            o::GET_FIELD => Instruction::GetField(u16::from_le_bytes(self.take()?)),
            o::STORE_FIELD => {
                let slot = u16::from_le_bytes(self.take()?);
                let count = u16::from_le_bytes(self.take()?);
                let mut steps = Vec::with_capacity(count as usize);
                for _ in 0..count {
                    steps.push(u16::from_le_bytes(self.take()?));
                }
                // `count` is a `u16`, so the path just read is encodable by
                // construction and this never takes the error arm — but it is
                // written as a `Result` rather than an unwrap, because a
                // decoder never gets to end its caller's process.
                let path = FieldPath::new(steps).map_err(|_| DecodeError::UnexpectedEnd {
                    offset: opcode_offset,
                })?;
                Instruction::StoreField { slot, path }
            }
            o::NEW_ARRAY => Instruction::NewArray(u32::from_le_bytes(self.take()?)),
            o::STORE_PLACE => {
                let (slot, path) = self.next_place(opcode_offset)?;
                Instruction::StorePlace { slot, path }
            }
            o::ARRAY_APPEND => {
                let (slot, path) = self.next_place(opcode_offset)?;
                Instruction::ArrayAppend { slot, path }
            }
            o::CALL_MUT => {
                let func = u32::from_le_bytes(self.take()?);
                let (slot, path) = self.next_place(opcode_offset)?;
                Instruction::CallMut { func, slot, path }
            }
            o::NEW_ENUM => {
                let tag = u16::from_le_bytes(self.take()?);
                let has_payload = self.take::<1>()?[0] != 0;
                Instruction::NewEnum { tag, has_payload }
            }
            other => nullary_from_opcode(other).ok_or(DecodeError::UnknownOpcode {
                opcode: other,
                offset: opcode_offset,
            })?,
        };
        Ok(instruction)
    }

    /// Decodes a place operand: slot, step count, then one tagged step each.
    ///
    /// An unknown step tag is rejected rather than guessed — a decoder never
    /// trusts its input, and a step it cannot name is a step it cannot walk.
    fn next_place(&mut self, opcode_offset: usize) -> Result<(u16, PlacePath), DecodeError> {
        let slot = u16::from_le_bytes(self.take()?);
        let count = u16::from_le_bytes(self.take()?);
        let mut steps = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let tag_offset = self.offset;
            let [tag] = self.take::<1>()?;
            steps.push(match tag {
                step_tag::FIELD => PathStep::Field(u16::from_le_bytes(self.take()?)),
                step_tag::INDEX => PathStep::Index,
                other => {
                    return Err(DecodeError::UnknownOpcode {
                        opcode: other,
                        offset: tag_offset,
                    });
                }
            });
        }
        // `count` is a `u16`, so the path just read is encodable by
        // construction and this never takes the error arm — but it is written
        // as a `Result` rather than an unwrap, because a decoder never gets to
        // end its caller's process.
        let path = PlacePath::new(steps).map_err(|_| DecodeError::UnexpectedEnd {
            offset: opcode_offset,
        })?;
        Ok((slot, path))
    }
}

/// The nullary instruction for an opcode, or `None` when the opcode carries
/// operands or is unknown.
fn nullary_from_opcode(op: u8) -> Option<Instruction> {
    Some(match op {
        o::CONST_VOID => Instruction::ConstVoid,
        o::POP => Instruction::Pop,
        o::NEG_INT => Instruction::NegInt,
        o::NEG_FLOAT => Instruction::NegFloat,
        o::NOT => Instruction::Not,
        o::ADD_INT => Instruction::AddInt,
        o::SUB_INT => Instruction::SubInt,
        o::MUL_INT => Instruction::MulInt,
        o::DIV_INT => Instruction::DivInt,
        o::REM_INT => Instruction::RemInt,
        o::DIV_UINT => Instruction::DivUInt,
        o::REM_UINT => Instruction::RemUInt,
        o::ADD_FLOAT => Instruction::AddFloat,
        o::SUB_FLOAT => Instruction::SubFloat,
        o::MUL_FLOAT => Instruction::MulFloat,
        o::DIV_FLOAT => Instruction::DivFloat,
        o::CONCAT_STR => Instruction::ConcatStr,
        o::EQ_INT => Instruction::EqInt,
        o::NE_INT => Instruction::NeInt,
        o::LT_INT => Instruction::LtInt,
        o::LE_INT => Instruction::LeInt,
        o::GT_INT => Instruction::GtInt,
        o::GE_INT => Instruction::GeInt,
        o::LT_UINT => Instruction::LtUInt,
        o::LE_UINT => Instruction::LeUInt,
        o::GT_UINT => Instruction::GtUInt,
        o::GE_UINT => Instruction::GeUInt,
        o::BIT_AND => Instruction::BitAnd,
        o::BIT_OR => Instruction::BitOr,
        o::BIT_XOR => Instruction::BitXor,
        o::SHL => Instruction::Shl,
        o::SHR_INT => Instruction::ShrInt,
        o::SHR_UINT => Instruction::ShrUInt,
        o::BIT_NOT => Instruction::BitNot,
        o::EQ_FLOAT => Instruction::EqFloat,
        o::NE_FLOAT => Instruction::NeFloat,
        o::LT_FLOAT => Instruction::LtFloat,
        o::LE_FLOAT => Instruction::LeFloat,
        o::GT_FLOAT => Instruction::GtFloat,
        o::GE_FLOAT => Instruction::GeFloat,
        o::EQ_BOOL => Instruction::EqBool,
        o::NE_BOOL => Instruction::NeBool,
        o::EQ_STR => Instruction::EqStr,
        o::NE_STR => Instruction::NeStr,
        o::ARRAY_GET => Instruction::ArrayGet,
        o::ARRAY_LEN => Instruction::ArrayLen,
        o::ENUM_TAG => Instruction::EnumTag,
        o::ENUM_PAYLOAD => Instruction::EnumPayload,
        o::CONVERT_INT_TO_FLOAT => Instruction::ConvertIntToFloat,
        o::CONVERT_FLOAT_TO_INT => Instruction::ConvertFloatToInt,
        o::PRINT => Instruction::Print,
        o::RETURN => Instruction::Return,
        o::RETURN_VOID => Instruction::ReturnVoid,
        _ => return None,
    })
}
