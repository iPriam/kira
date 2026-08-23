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

use super::{
    CompilerOp, EnvOp, FieldPath, FileSystemOp, Instruction, MathOp, PathStep, PlacePath, StringOp,
    TaskPrim, WritebackTarget, opcode as o, step_tag,
};

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
    /// A boolean operand was neither its canonical false nor true byte.
    #[error("invalid boolean byte {value:#04x} at offset {offset}")]
    InvalidBoolean {
        /// The byte found on the wire.
        value: u8,
        /// Byte offset of the invalid operand.
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
        Instruction::CallWriteback { func, targets } => {
            out.push(o::CALL_WRITEBACK);
            out.extend_from_slice(&func.to_le_bytes());
            out.extend_from_slice(&(targets.len() as u64).to_le_bytes());
            for target in targets {
                out.extend_from_slice(&target.param.to_le_bytes());
                encode_place(target.slot, &target.path, out);
            }
        }
        Instruction::CallNativeWriteback { func, targets } => {
            out.push(o::CALL_NATIVE_WRITEBACK);
            out.extend_from_slice(&func.to_le_bytes());
            out.extend_from_slice(&(targets.len() as u64).to_le_bytes());
            for target in targets {
                out.extend_from_slice(&target.param.to_le_bytes());
                encode_place(target.slot, &target.path, out);
            }
        }
        Instruction::NativeState(type_id) => {
            out.push(o::NATIVE_STATE);
            out.extend_from_slice(&type_id.to_le_bytes());
        }
        Instruction::NativeRecover(type_id) => {
            out.push(o::NATIVE_RECOVER);
            out.extend_from_slice(&type_id.to_le_bytes());
        }
        Instruction::NewStruct(fields) => {
            out.push(o::NEW_STRUCT);
            out.extend_from_slice(&fields.to_le_bytes());
        }
        Instruction::TakeLocal(slot) => {
            out.push(o::TAKE_LOCAL);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::NewStructDropping { fields, glue } => {
            out.push(o::NEW_STRUCT_DROPPING);
            out.extend_from_slice(&fields.to_le_bytes());
            out.extend_from_slice(&glue.to_le_bytes());
        }
        Instruction::GetField(index) => {
            out.push(o::GET_FIELD);
            out.extend_from_slice(&index.to_le_bytes());
        }
        Instruction::ForeignOffset(offset) => {
            out.push(o::FOREIGN_OFFSET);
            out.extend_from_slice(&offset.to_le_bytes());
        }
        Instruction::ForeignIndex(stride) => {
            out.push(o::FOREIGN_INDEX);
            out.extend_from_slice(&stride.to_le_bytes());
        }
        Instruction::ForeignLoad { offset, ty } => {
            out.push(o::FOREIGN_LOAD);
            out.extend_from_slice(&offset.to_le_bytes());
            out.push(ty.tag());
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
        Instruction::StringLen => out.push(o::STRING_LEN),
        Instruction::StringCharAt => out.push(o::STRING_CHAR_AT),
        Instruction::StringSubstring => out.push(o::STRING_SUBSTRING),
        Instruction::StringIndexOf => out.push(o::STRING_INDEX_OF),
        Instruction::StringOf => out.push(o::STRING_OF),
        Instruction::ConvertFloatToBits => out.push(o::CONVERT_FLOAT_TO_BITS),
        Instruction::ConvertBitsToFloat => out.push(o::CONVERT_BITS_TO_FLOAT),
        Instruction::ConvertBits32ToFloat => out.push(o::CONVERT_BITS32_TO_FLOAT),
        Instruction::ConvertFloatToBits32 => out.push(o::CONVERT_FLOAT_TO_BITS32),
        Instruction::ArrayGetLocal(slot) => {
            out.push(o::ARRAY_GET_LOCAL);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::CellGet(slot) => {
            out.push(o::CELL_GET);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::CellSet(slot) => {
            out.push(o::CELL_SET);
            out.extend_from_slice(&slot.to_le_bytes());
        }
        Instruction::TaskOp(prim) => {
            out.push(o::TASK_OP);
            out.push(prim.as_byte());
        }
        Instruction::CStringNew => out.push(o::CSTRING_NEW),
        Instruction::CLayoutAddress(aggregate) => {
            out.push(o::CLAYOUT_ADDRESS);
            out.extend_from_slice(&aggregate.to_le_bytes());
        }
        Instruction::FileSystem(op) => {
            out.push(o::FILE_SYSTEM);
            out.push(op.as_byte());
        }
        Instruction::StringOp(op) => {
            out.push(o::STRING_OP);
            out.push(op.as_byte());
        }
        Instruction::ScalarText => out.push(o::SCALAR_TEXT),
        Instruction::ArrayElements(ty) => {
            out.push(o::ARRAY_ELEMENTS);
            out.push(ty.tag());
        }
        Instruction::MathOp(op) => {
            out.push(o::MATH_OP);
            out.push(op.tag());
        }
        Instruction::Compiler(op) => {
            out.push(o::COMPILER);
            out.push(op.as_byte());
        }
        Instruction::Env(op) => {
            out.push(o::ENV_OP);
            out.push(op.as_byte());
        }
        Instruction::NewCell => out.push(o::NEW_CELL),
        Instruction::EnumTag => out.push(o::ENUM_TAG),
        Instruction::EnumPayload => out.push(o::ENUM_PAYLOAD),
        Instruction::ConvertIntToFloat => out.push(o::CONVERT_INT_TO_FLOAT),
        Instruction::ConvertFloatToInt => out.push(o::CONVERT_FLOAT_TO_INT),
        Instruction::ConvertIntToRawPtr => out.push(o::CONVERT_INT_TO_RAW_PTR),
        Instruction::ConvertRawPtrToInt => out.push(o::CONVERT_RAW_PTR_TO_INT),
        Instruction::NativeUserData => out.push(o::NATIVE_USER_DATA),
        Instruction::NativeStateFree => out.push(o::NATIVE_STATE_FREE),
        Instruction::ConstRawPtrNull => out.push(o::RAW_PTR_NULL),
        Instruction::ForeignCallback(id) => {
            out.push(o::FOREIGN_CALLBACK);
            out.extend_from_slice(&id.to_le_bytes());
        }
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
        Instruction::RemFloat => out.push(o::REM_FLOAT),
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
        Instruction::Erase(type_id) => {
            out.push(o::ERASE);
            out.extend_from_slice(&type_id.to_le_bytes());
        }
        Instruction::EqAny => out.push(o::EQ_ANY),
        Instruction::NeAny => out.push(o::NE_ANY),
        Instruction::EqStr => out.push(o::EQ_STR),
        Instruction::NeStr => out.push(o::NE_STR),
        Instruction::Print => out.push(o::PRINT),
        Instruction::Return => out.push(o::RETURN),
        Instruction::ReturnVoid => out.push(o::RETURN_VOID),
    }
}

/// Appends a place operand — slot, step count, then one tagged step each.
fn encode_place(slot: u64, path: &PlacePath, out: &mut Vec<u8>) {
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
    decode_with_width(bytes, false)
}

/// Decodes the instruction widths used by a KBC1 module.
pub(crate) fn decode_legacy(bytes: &[u8]) -> Result<Vec<Instruction>, DecodeError> {
    decode_with_width(bytes, true)
}

fn decode_with_width(bytes: &[u8], legacy: bool) -> Result<Vec<Instruction>, DecodeError> {
    let mut cursor = Cursor { bytes, offset: 0 };
    let mut code = Vec::new();
    while cursor.offset < bytes.len() {
        code.push(cursor.next_instruction(legacy)?);
    }
    Ok(code)
}

struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl Cursor<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let end = self
            .offset
            .checked_add(N)
            .ok_or(DecodeError::UnexpectedEnd {
                offset: self.offset,
            })?;
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

    fn read_word(&mut self, legacy: bool) -> Result<u64, DecodeError> {
        if legacy {
            Ok(u64::from(u32::from_le_bytes(self.take()?)))
        } else {
            Ok(u64::from_le_bytes(self.take()?))
        }
    }

    fn read_slot(&mut self, legacy: bool) -> Result<u64, DecodeError> {
        if legacy {
            Ok(u64::from(u16::from_le_bytes(self.take()?)))
        } else {
            Ok(u64::from_le_bytes(self.take()?))
        }
    }

    fn read_bool(&mut self) -> Result<bool, DecodeError> {
        let offset = self.offset;
        let [value] = self.take()?;
        match value {
            0 => Ok(false),
            1 => Ok(true),
            value => Err(DecodeError::InvalidBoolean { value, offset }),
        }
    }

    fn next_instruction(&mut self, legacy: bool) -> Result<Instruction, DecodeError> {
        let opcode_offset = self.offset;
        let [op] = self.take::<1>()?;
        let instruction = match op {
            o::CONST_INT => Instruction::ConstInt(i64::from_le_bytes(self.take()?)),
            o::CONST_FLOAT => Instruction::ConstFloat(f64::from_le_bytes(self.take()?)),
            o::CONST_BOOL => Instruction::ConstBool(self.read_bool()?),
            o::CONST_STR => Instruction::ConstStr(self.read_word(legacy)?),
            o::ERASE => Instruction::Erase(u64::from_le_bytes(self.take()?)),
            o::CONST_VOID => Instruction::ConstVoid,
            o::LOAD_LOCAL => Instruction::LoadLocal(self.read_slot(legacy)?),
            o::ARRAY_GET_LOCAL => Instruction::ArrayGetLocal(self.read_slot(legacy)?),
            o::CELL_GET => Instruction::CellGet(self.read_slot(legacy)?),
            o::CELL_SET => Instruction::CellSet(self.read_slot(legacy)?),
            o::STORE_LOCAL => Instruction::StoreLocal(self.read_slot(legacy)?),
            o::JUMP => Instruction::Jump(self.read_word(legacy)?),
            o::JUMP_IF_FALSE => Instruction::JumpIfFalse(self.read_word(legacy)?),
            o::CALL => Instruction::Call(self.read_word(legacy)?),
            o::CALL_NATIVE => Instruction::CallNative(u32::from_le_bytes(self.take()?)),
            o::CALL_FOREIGN => Instruction::CallForeign(u32::from_le_bytes(self.take()?)),
            o::FOREIGN_CALLBACK => Instruction::ForeignCallback(u32::from_le_bytes(self.take()?)),
            o::NATIVE_STATE => Instruction::NativeState(u64::from_le_bytes(self.take()?)),
            o::NATIVE_RECOVER => Instruction::NativeRecover(u64::from_le_bytes(self.take()?)),
            o::NEW_STRUCT => Instruction::NewStruct(self.read_slot(legacy)?),
            o::TAKE_LOCAL => Instruction::TakeLocal(self.read_slot(legacy)?),
            o::NEW_STRUCT_DROPPING => {
                let fields = self.read_slot(legacy)?;
                Instruction::NewStructDropping {
                    fields,
                    glue: u32::from_le_bytes(self.take()?),
                }
            }
            o::GET_FIELD => Instruction::GetField(self.read_slot(legacy)?),
            o::FOREIGN_OFFSET => Instruction::ForeignOffset(u32::from_le_bytes(self.take()?)),
            o::FOREIGN_INDEX => Instruction::ForeignIndex(u32::from_le_bytes(self.take()?)),
            o::FOREIGN_LOAD => {
                let offset = u32::from_le_bytes(self.take()?);
                let at = self.offset;
                let [tag] = self.take()?;
                let ty = kira_runtime_abi::ForeignType::from_tag(tag).ok_or(
                    DecodeError::UnknownOpcode {
                        opcode: tag,
                        offset: at,
                    },
                )?;
                Instruction::ForeignLoad { offset, ty }
            }
            o::STORE_FIELD => {
                let slot = self.read_slot(legacy)?;
                let count = self.read_slot(legacy)?;
                let mut steps = Vec::new();
                for _ in 0..count {
                    steps.push(self.read_slot(legacy)?);
                }
                let path = FieldPath::new(steps);
                Instruction::StoreField { slot, path }
            }
            o::NEW_ARRAY => Instruction::NewArray(self.read_word(legacy)?),
            o::STORE_PLACE => {
                let (slot, path) = self.next_place(legacy)?;
                Instruction::StorePlace { slot, path }
            }
            o::ARRAY_APPEND => {
                let (slot, path) = self.next_place(legacy)?;
                Instruction::ArrayAppend { slot, path }
            }
            o::CALL_MUT => {
                let func = self.read_word(legacy)?;
                let (slot, path) = self.next_place(legacy)?;
                Instruction::CallMut { func, slot, path }
            }
            o::CALL_WRITEBACK | o::CALL_NATIVE_WRITEBACK => {
                let native = op == o::CALL_NATIVE_WRITEBACK;
                let (func, native_func) = if native {
                    (0, u32::from_le_bytes(self.take()?))
                } else {
                    (self.read_word(legacy)?, 0)
                };
                let count = self.read_slot(legacy)?;
                let mut targets = Vec::new();
                for _ in 0..count {
                    let param = self.read_slot(legacy)?;
                    let (slot, path) = self.next_place(legacy)?;
                    targets.push(WritebackTarget { param, slot, path });
                }
                if !native {
                    Instruction::CallWriteback { func, targets }
                } else {
                    Instruction::CallNativeWriteback {
                        func: native_func,
                        targets,
                    }
                }
            }
            o::NEW_ENUM => {
                let tag = self.read_slot(legacy)?;
                let has_payload = self.read_bool()?;
                Instruction::NewEnum { tag, has_payload }
            }
            o::CLAYOUT_ADDRESS => Instruction::CLayoutAddress(u32::from_le_bytes(self.take()?)),
            o::FILE_SYSTEM => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let op = FileSystemOp::from_byte(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::FileSystem(op)
            }
            o::STRING_OP => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let op = StringOp::from_byte(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::StringOp(op)
            }
            o::SCALAR_TEXT => Instruction::ScalarText,
            o::ARRAY_ELEMENTS => {
                let at = self.offset;
                let [tag] = self.take()?;
                let ty = kira_runtime_abi::ForeignType::from_tag(tag).ok_or(
                    DecodeError::UnknownOpcode {
                        opcode: tag,
                        offset: at,
                    },
                )?;
                Instruction::ArrayElements(ty)
            }
            o::MATH_OP => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let op = MathOp::from_tag(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::MathOp(op)
            }
            o::COMPILER => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let op = CompilerOp::from_byte(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::Compiler(op)
            }
            o::ENV_OP => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let op = EnvOp::from_byte(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::Env(op)
            }
            o::TASK_OP => {
                let tag_offset = self.offset;
                let [tag] = self.take::<1>()?;
                let prim = TaskPrim::from_byte(tag).ok_or(DecodeError::UnknownOpcode {
                    opcode: tag,
                    offset: tag_offset,
                })?;
                Instruction::TaskOp(prim)
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
    fn next_place(&mut self, legacy: bool) -> Result<(u64, PlacePath), DecodeError> {
        let slot = self.read_slot(legacy)?;
        let count = self.read_slot(legacy)?;
        let mut steps = Vec::new();
        for _ in 0..count {
            let tag_offset = self.offset;
            let [tag] = self.take::<1>()?;
            steps.push(match tag {
                step_tag::FIELD => PathStep::Field(self.read_slot(legacy)?),
                step_tag::INDEX => PathStep::Index,
                other => {
                    return Err(DecodeError::UnknownOpcode {
                        opcode: other,
                        offset: tag_offset,
                    });
                }
            });
        }
        let path = PlacePath::new(steps);
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
        o::REM_FLOAT => Instruction::RemFloat,
        o::NEW_CELL => Instruction::NewCell,
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
        o::EQ_ANY => Instruction::EqAny,
        o::NE_ANY => Instruction::NeAny,
        o::EQ_STR => Instruction::EqStr,
        o::NE_STR => Instruction::NeStr,
        o::ARRAY_GET => Instruction::ArrayGet,
        o::ARRAY_LEN => Instruction::ArrayLen,
        o::STRING_LEN => Instruction::StringLen,
        o::STRING_CHAR_AT => Instruction::StringCharAt,
        o::STRING_SUBSTRING => Instruction::StringSubstring,
        o::STRING_INDEX_OF => Instruction::StringIndexOf,
        o::STRING_OF => Instruction::StringOf,
        o::CONVERT_FLOAT_TO_BITS => Instruction::ConvertFloatToBits,
        o::CONVERT_BITS_TO_FLOAT => Instruction::ConvertBitsToFloat,
        o::CONVERT_BITS32_TO_FLOAT => Instruction::ConvertBits32ToFloat,
        o::CONVERT_FLOAT_TO_BITS32 => Instruction::ConvertFloatToBits32,

        o::CSTRING_NEW => Instruction::CStringNew,
        o::ENUM_TAG => Instruction::EnumTag,
        o::ENUM_PAYLOAD => Instruction::EnumPayload,
        o::CONVERT_INT_TO_FLOAT => Instruction::ConvertIntToFloat,
        o::CONVERT_FLOAT_TO_INT => Instruction::ConvertFloatToInt,
        o::CONVERT_INT_TO_RAW_PTR => Instruction::ConvertIntToRawPtr,
        o::CONVERT_RAW_PTR_TO_INT => Instruction::ConvertRawPtrToInt,
        o::NATIVE_USER_DATA => Instruction::NativeUserData,
        o::NATIVE_STATE_FREE => Instruction::NativeStateFree,
        o::RAW_PTR_NULL => Instruction::ConstRawPtrNull,
        o::PRINT => Instruction::Print,
        o::RETURN => Instruction::Return,
        o::RETURN_VOID => Instruction::ReturnVoid,
        _ => return None,
    })
}
