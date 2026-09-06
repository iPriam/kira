//! Integer width and conversion semantics, as instructions.
//!
//! Every engine stores an integer as 64 bits, but the language gives each
//! written spelling its own range: arithmetic that leaves the range traps, a
//! shift count is measured against the width, and a conversion between
//! spellings traps when the value does not fit. The instructions that carry
//! those rules are emitted here, after the operands, so the operator tables
//! in [`super`] stay about which opcode, not which width.

use kira_ir::{ConvertKind, IrBinOp};
use kira_semantics_model::{IntSpelling, Type};

use super::{CompileError, FnCompiler, binary_instruction};
use crate::op::Instruction;

impl FnCompiler<'_> {
    /// Emits an integer or other binary operator whose operands are already
    /// on the stack, plus the width checks the result type asks for.
    pub(super) fn compile_int_operator(
        &mut self,
        op: IrBinOp,
        ty: Type,
    ) -> Result<(), CompileError> {
        let spelling = match ty {
            Type::Int(spelling) => spelling,
            _ => {
                self.code.push(binary_instruction(op)?);
                return Ok(());
            }
        };
        match op {
            // Overflow past 64 bits is the instruction's own trap; at a
            // narrower width the 64-bit result is then range-checked.
            IrBinOp::AddInt | IrBinOp::SubInt | IrBinOp::MulInt => {
                let instruction = match (op, spelling) {
                    (IrBinOp::AddInt, IntSpelling::U64) => Instruction::AddUIntChecked,
                    (IrBinOp::SubInt, IntSpelling::U64) => Instruction::SubUIntChecked,
                    (IrBinOp::MulInt, IntSpelling::U64) => Instruction::MulUIntChecked,
                    _ => binary_instruction(op)?,
                };
                self.code.push(instruction);
                self.check_width(ty);
            }
            // `MIN / -1` is the one overflowing division, at every signed
            // width: `I8(-128) / -1` is 128.
            IrBinOp::DivInt => {
                self.code.push(Instruction::DivIntChecked);
                self.check_width(ty);
            }
            // The count is checked against the width first; a left shift then
            // discards the bits it pushed out of the width.
            IrBinOp::Shl | IrBinOp::ShrInt | IrBinOp::ShrUInt => {
                self.code
                    .push(Instruction::CheckShift(spelling.bits() as u8));
                self.code.push(binary_instruction(op)?);
                if op == IrBinOp::Shl {
                    self.wrap_width(ty);
                }
            }
            // Explicit wrapping arithmetic wraps at the written width, not at
            // 64 bits.
            IrBinOp::WrappingAddInt | IrBinOp::WrappingSubInt | IrBinOp::WrappingMulInt => {
                self.code.push(binary_instruction(op)?);
                self.wrap_width(ty);
            }
            _ => self.code.push(binary_instruction(op)?),
        }
        Ok(())
    }

    /// Range-checks the 64-bit result on the stack against `ty`'s width, when
    /// that width is narrower than the word.
    pub(super) fn check_width(&mut self, ty: Type) {
        if let Type::Int(spelling) = ty
            && spelling.bits() < 64
        {
            self.code.push(Instruction::CheckInt(spelling.code()));
        }
    }

    /// Reduces the result on the stack to `ty`'s width, when that width is
    /// narrower than the word.
    fn wrap_width(&mut self, ty: Type) {
        if let Type::Int(spelling) = ty
            && spelling.bits() < 64
        {
            self.code.push(Instruction::WrapInt(spelling.code()));
        }
    }

    /// Emits a scalar conversion of the value on the stack, from `from` to
    /// `to`.
    ///
    /// An integer-to-integer conversion is an identity copy when every value
    /// of the source fits the destination, and a checked narrowing otherwise.
    /// A float-to-integer conversion traps on NaN, infinity, and a value
    /// outside the destination's range.
    pub(super) fn compile_convert(&mut self, kind: ConvertKind, from: Type, to: Type) {
        let from_spelling = match from {
            Type::Int(spelling) => spelling,
            _ => IntSpelling::Plain,
        };
        match kind {
            ConvertKind::IntToInt => {
                if let Type::Int(to_spelling) = to
                    && !from_spelling.widens_into(to_spelling)
                {
                    self.code.push(Instruction::ConvertInt {
                        from: from_spelling.code(),
                        to: to_spelling.code(),
                    });
                }
            }
            // A float width is an annotation over one representation.
            ConvertKind::FloatToFloat => {}
            ConvertKind::IntToRawPtr => self.code.push(Instruction::ConvertIntToRawPtr),
            ConvertKind::RawPtrToInt => self.code.push(Instruction::ConvertRawPtrToInt),
            ConvertKind::IntToFloat => self.code.push(if from_spelling == IntSpelling::U64 {
                Instruction::ConvertUIntToFloat
            } else {
                Instruction::ConvertIntToFloat
            }),
            ConvertKind::FloatToInt => {
                self.code.push(Instruction::ConvertFloatToInt);
                if let Type::Int(to_spelling) = to
                    && to_spelling != IntSpelling::Plain
                {
                    self.code.push(Instruction::ConvertInt {
                        from: IntSpelling::Plain.code(),
                        to: to_spelling.code(),
                    });
                }
            }
            ConvertKind::FloatToBits => self.code.push(Instruction::ConvertFloatToBits),
            ConvertKind::BitsToFloat => self.code.push(Instruction::ConvertBitsToFloat),
            ConvertKind::Bits32ToFloat => self.code.push(Instruction::ConvertBits32ToFloat),
            ConvertKind::FloatToBits32 => self.code.push(Instruction::ConvertFloatToBits32),
        }
    }
}
