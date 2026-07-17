//! The Kira VM instruction set and its byte encoding.
//!
//! The interpreter executes the decoded [`Instruction`] form (match-in-loop),
//! while [`encode`]/[`decode`] provide the on-the-wire byte format. Opcodes are
//! Kira-owned, closed tags with explicit `u8` discriminants; the encoding is
//! **append-only** — new instructions take the next free opcode and existing
//! ones never move.
//!
//! Jump targets are absolute instruction indices within a function's code, so
//! the interpreter sets its program counter directly with no offset math.

/// One decoded VM instruction.
#[derive(Debug, Clone, PartialEq)]
pub enum Instruction {
    /// Push an integer constant.
    ConstInt(i64),
    /// Push a floating-point constant.
    ConstFloat(f64),
    /// Push a boolean constant.
    ConstBool(bool),
    /// Push a fresh heap string cloned from the module's string pool.
    ConstStr(u32),
    /// Push the unit value.
    ConstVoid,
    /// Push a copy of local slot `n` (strings are cloned).
    LoadLocal(u16),
    /// Pop the stack top into local slot `n`, dropping the slot's old value.
    StoreLocal(u16),
    /// Pop and drop the stack top.
    Pop,
    /// Integer negation.
    NegInt,
    /// Float negation.
    NegFloat,
    /// Boolean negation.
    Not,
    /// Integer addition.
    AddInt,
    /// Integer subtraction.
    SubInt,
    /// Integer multiplication.
    MulInt,
    /// Integer division (truncating; traps on divide-by-zero).
    DivInt,
    /// Integer remainder (traps on divide-by-zero).
    RemInt,
    /// Float addition.
    AddFloat,
    /// Float subtraction.
    SubFloat,
    /// Float multiplication.
    MulFloat,
    /// Float division.
    DivFloat,
    /// String concatenation.
    ConcatStr,
    /// Integer equality.
    EqInt,
    /// Integer inequality.
    NeInt,
    /// Integer less-than.
    LtInt,
    /// Integer less-or-equal.
    LeInt,
    /// Integer greater-than.
    GtInt,
    /// Integer greater-or-equal.
    GeInt,
    /// Float equality.
    EqFloat,
    /// Float inequality.
    NeFloat,
    /// Float less-than.
    LtFloat,
    /// Float less-or-equal.
    LeFloat,
    /// Float greater-than.
    GtFloat,
    /// Float greater-or-equal.
    GeFloat,
    /// Boolean equality.
    EqBool,
    /// Boolean inequality.
    NeBool,
    /// String equality.
    EqStr,
    /// String inequality.
    NeStr,
    /// Unconditional jump to an absolute instruction index.
    Jump(u32),
    /// Pop a boolean; jump to an absolute index when it is `false`.
    JumpIfFalse(u32),
    /// Call the function at the given index; arguments are already on the stack.
    Call(u32),
    /// Call the *native* function with the given program-wide id; arguments are
    /// already on the stack, and the result is pushed.
    ///
    /// Emitted only by a hybrid build, where the callee's body lives in the
    /// native half rather than in this module's function table. The VM does not
    /// perform the call itself — it asks the embedder, which keeps the VM free
    /// of any FFI and still able to compile for wasm.
    CallNative(u32),
    /// Pop a value, format it, emit one output line, and push unit.
    Print,
    /// Return the stack top from the current function.
    Return,
    /// Return unit from the current function.
    ReturnVoid,
    /// Pop `n` values and push a struct holding them, first field deepest.
    ///
    /// The VM is structurally typed: a struct is a tuple of values and this
    /// carries its own arity, so the module needs no struct table and field
    /// names never reach the runtime. The compiler resolves names to indices
    /// and fills every field — defaults included — before emitting this.
    NewStruct(u16),
    /// Pop a struct, push a copy of field `n`, and drop the struct.
    GetField(u16),
    /// Pop a value and store it into local `slot`, walking `path` field by
    /// field from the slot's struct. The overwritten value is dropped.
    ///
    /// The path is carried in the instruction rather than rebuilt from loads
    /// and stores so a nested write mutates in place: `b.size.x = 1` costs one
    /// instruction and no copy of `b`.
    StoreField {
        /// The local slot the place is rooted at.
        slot: u16,
        /// Field indices to walk, outermost first; never empty.
        path: FieldPath,
    },
}

/// A field path inside a [`Instruction::StoreField`], short enough to encode.
///
/// The length is a `u16` on the wire, so a path is capped at `u16::MAX` steps.
/// The cap lives in the one constructor and the steps are private, which is
/// what makes [`encode_one`] total: an unencodable path cannot be built, so
/// encoding never has to truncate one and never has to fail.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldPath {
    steps: Vec<u16>,
}

/// A field path with more steps than the bytecode format can encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("a field path of {count} steps exceeds the bytecode format's 65535")]
pub struct FieldPathTooDeep {
    /// How many steps were requested.
    pub count: usize,
}

impl FieldPath {
    /// Builds a path, or fails when it is too deep to encode.
    pub fn new(steps: Vec<u16>) -> Result<Self, FieldPathTooDeep> {
        if u16::try_from(steps.len()).is_err() {
            return Err(FieldPathTooDeep { count: steps.len() });
        }
        Ok(Self { steps })
    }

    /// The steps to walk, outermost first.
    pub fn steps(&self) -> &[u16] {
        &self.steps
    }

    /// How many steps the path walks. Always fits in a `u16`.
    pub fn len(&self) -> u16 {
        // Guaranteed by the only constructor.
        self.steps.len() as u16
    }

    /// Whether the path walks no steps.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty()
    }
}

/// The opcode byte for each instruction. Append-only: never reorder or reuse.
mod opcode {
    pub const CONST_INT: u8 = 0x01;
    pub const CONST_FLOAT: u8 = 0x02;
    pub const CONST_BOOL: u8 = 0x03;
    pub const CONST_STR: u8 = 0x04;
    pub const CONST_VOID: u8 = 0x05;
    pub const LOAD_LOCAL: u8 = 0x06;
    pub const STORE_LOCAL: u8 = 0x07;
    pub const POP: u8 = 0x08;
    pub const NEG_INT: u8 = 0x09;
    pub const NEG_FLOAT: u8 = 0x0a;
    pub const NOT: u8 = 0x0b;
    pub const ADD_INT: u8 = 0x0c;
    pub const SUB_INT: u8 = 0x0d;
    pub const MUL_INT: u8 = 0x0e;
    pub const DIV_INT: u8 = 0x0f;
    pub const REM_INT: u8 = 0x10;
    pub const ADD_FLOAT: u8 = 0x11;
    pub const SUB_FLOAT: u8 = 0x12;
    pub const MUL_FLOAT: u8 = 0x13;
    pub const DIV_FLOAT: u8 = 0x14;
    pub const CONCAT_STR: u8 = 0x15;
    pub const EQ_INT: u8 = 0x16;
    pub const NE_INT: u8 = 0x17;
    pub const LT_INT: u8 = 0x18;
    pub const LE_INT: u8 = 0x19;
    pub const GT_INT: u8 = 0x1a;
    pub const GE_INT: u8 = 0x1b;
    pub const EQ_FLOAT: u8 = 0x1c;
    pub const NE_FLOAT: u8 = 0x1d;
    pub const LT_FLOAT: u8 = 0x1e;
    pub const LE_FLOAT: u8 = 0x1f;
    pub const GT_FLOAT: u8 = 0x20;
    pub const GE_FLOAT: u8 = 0x21;
    pub const EQ_BOOL: u8 = 0x22;
    pub const NE_BOOL: u8 = 0x23;
    pub const EQ_STR: u8 = 0x24;
    pub const NE_STR: u8 = 0x25;
    pub const JUMP: u8 = 0x26;
    pub const JUMP_IF_FALSE: u8 = 0x27;
    pub const CALL: u8 = 0x28;
    pub const PRINT: u8 = 0x29;
    pub const RETURN: u8 = 0x2a;
    pub const RETURN_VOID: u8 = 0x2b;
    pub const CALL_NATIVE: u8 = 0x2c;
    pub const NEW_STRUCT: u8 = 0x2d;
    pub const GET_FIELD: u8 = 0x2e;
    pub const STORE_FIELD: u8 = 0x2f;
}

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
    use opcode as o;
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
        // Nullary instructions: one exhaustive arm each, so encoding is total
        // by construction (no fallthrough, no panic path).
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
        use opcode as o;
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
            other => nullary_from_opcode(other).ok_or(DecodeError::UnknownOpcode {
                opcode: other,
                offset: opcode_offset,
            })?,
        };
        Ok(instruction)
    }
}

/// The nullary instruction for an opcode, or `None` when the opcode carries
/// operands or is unknown.
fn nullary_from_opcode(op: u8) -> Option<Instruction> {
    use opcode as o;
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
        o::PRINT => Instruction::Print,
        o::RETURN => Instruction::Return,
        o::RETURN_VOID => Instruction::ReturnVoid,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
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
}
