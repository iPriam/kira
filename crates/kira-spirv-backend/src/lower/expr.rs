//! Lowering the expressions that are more than one instruction.
//!
//! An operator's opcode depends on what it operates on — a matrix times a
//! vector is not a componentwise multiply, and `min` over floats is a different
//! instruction from `min` over unsigned integers — so the choosing lives here,
//! beside the tables in [`ops`](super::ops) that answer it.

use kira_ksl_semantics::model::{BinaryOp, BuiltinFn, CheckedExprId};
use kira_shader_model::{ScalarType, Type};

use crate::Emitter;
use crate::builder::Id;
use crate::lower::ops::{element_scalar, extended_instruction, opcode_for};
use crate::spec::{op, scope};

/// The image-operands mask naming an explicit level of detail.
const LOD_OPERAND: u32 = 0x0000_0002;

impl Emitter<'_> {
    /// Emits a binary operation, choosing the opcode by what it operates on.
    pub(crate) fn binary(
        &mut self,
        operator: BinaryOp,
        lhs: CheckedExprId,
        rhs: CheckedExprId,
        result: &Type,
    ) -> Id {
        let lhs_ty = self.module.expr(lhs).ty.clone();
        let rhs_ty = self.module.expr(rhs).ty.clone();
        let lhs = self.value(lhs);
        let rhs = self.value(rhs);
        let result_id = self.ty(result);
        let out = self.builder.fresh();

        // A matrix times a vector is not a componentwise multiply, and neither
        // is a vector times a scalar: SPIR-V spells all three differently.
        if operator == BinaryOp::Mul {
            let opcode = match (&lhs_ty, &rhs_ty) {
                (Type::Matrix(_), Type::Vector(_)) => Some(op::MATRIX_TIMES_VECTOR),
                (Type::Matrix(_), Type::Matrix(_)) => Some(op::MATRIX_TIMES_MATRIX),
                (Type::Vector(_), Type::Scalar(ScalarType::Float)) => Some(op::VECTOR_TIMES_SCALAR),
                _ => None,
            };
            if let Some(opcode) = opcode {
                self.builder.code(
                    opcode,
                    &[result_id.word(), out.word(), lhs.word(), rhs.word()],
                );
                return out;
            }
            // A scalar on the left is the same product with its operands the
            // other way round, and SPIR-V takes the vector first.
            if let (Type::Scalar(ScalarType::Float), Type::Vector(_)) = (&lhs_ty, &rhs_ty) {
                self.builder.code(
                    op::VECTOR_TIMES_SCALAR,
                    &[result_id.word(), out.word(), rhs.word(), lhs.word()],
                );
                return out;
            }
        }

        let scalar = element_scalar(&lhs_ty).unwrap_or(ScalarType::Float);
        let opcode = opcode_for(operator, scalar);
        self.builder.code(
            opcode,
            &[result_id.word(), out.word(), lhs.word(), rhs.word()],
        );
        out
    }

    /// Converts `value` from `source` to `target`.
    pub(crate) fn convert(&mut self, value: Id, source: &Type, target: &Type) -> Id {
        let from = element_scalar(source);
        let to = element_scalar(target);
        let (Some(from), Some(to)) = (from, to) else {
            return value;
        };
        if from == to {
            return value;
        }
        let opcode = match (from, to) {
            (ScalarType::Float, ScalarType::Int) => op::CONVERT_F_TO_S,
            (ScalarType::Float, ScalarType::Uint) => op::CONVERT_F_TO_U,
            (ScalarType::Int, ScalarType::Float) => op::CONVERT_S_TO_F,
            (ScalarType::Uint, ScalarType::Float) => op::CONVERT_U_TO_F,
            // Same width, different signedness: a reinterpretation, and SPIR-V
            // says so rather than pretending a value changed.
            (ScalarType::Int, ScalarType::Uint) | (ScalarType::Uint, ScalarType::Int) => {
                op::BITCAST
            }
            // Bool converts to nothing here — KSL has no cast to or from it —
            // and the equal-scalar pairs already returned above.
            _ => return value,
        };
        let result = self.ty(target);
        let out = self.builder.fresh();
        self.builder
            .code(opcode, &[result.word(), out.word(), value.word()]);
        out
    }

    /// Emits one builtin call.
    pub(crate) fn builtin(
        &mut self,
        which: BuiltinFn,
        args: &[CheckedExprId],
        result: &Type,
    ) -> Id {
        match which {
            BuiltinFn::Mul => {
                let (Some(&lhs), Some(&rhs)) = (args.first(), args.get(1)) else {
                    return self.undefined(result);
                };
                self.binary(BinaryOp::Mul, lhs, rhs, result)
            }
            BuiltinFn::Sample => self.sample(args, result),
            BuiltinFn::Load => self.fetch(args, result),
            BuiltinFn::AtomicAdd => self.atomic_add(args, result),
            other => self.extended(other, args, result),
        }
    }

    /// `sample(texture, sampler, uv)`.
    fn sample(&mut self, args: &[CheckedExprId], result: &Type) -> Id {
        let (Some(&texture), Some(&sampler), Some(&coordinate)) =
            (args.first(), args.get(1), args.get(2))
        else {
            return self.undefined(result);
        };
        let image = self.value(texture);
        let sampler = self.value(sampler);
        let coordinate = self.value(coordinate);
        let image_ty = self.handle_type(texture);
        let combined_ty = self.sampled_image(image_ty);
        let combined = self.builder.fresh();
        self.builder.code(
            op::SAMPLED_IMAGE,
            &[
                combined_ty.word(),
                combined.word(),
                image.word(),
                sampler.word(),
            ],
        );
        let result_id = self.ty(result);
        let out = self.builder.fresh();
        self.builder.code(
            op::IMAGE_SAMPLE_IMPLICIT_LOD,
            &[
                result_id.word(),
                out.word(),
                combined.word(),
                coordinate.word(),
            ],
        );
        out
    }

    /// `load(texture, coordinate)`.
    fn fetch(&mut self, args: &[CheckedExprId], result: &Type) -> Id {
        let (Some(&texture), Some(&coordinate)) = (args.first(), args.get(1)) else {
            return self.undefined(result);
        };
        let image = self.value(texture);
        let coordinate = self.value(coordinate);
        let result_id = self.ty(result);
        let level = self.uint(0);
        let out = self.builder.fresh();
        // The `Lod` image operand is not optional for a sampled image, and KSL's
        // `load` names no level — so level 0 is said explicitly.
        self.builder.code(
            op::IMAGE_FETCH,
            &[
                result_id.word(),
                out.word(),
                image.word(),
                coordinate.word(),
                LOD_OPERAND,
                level.word(),
            ],
        );
        out
    }

    /// `atomicAdd(buffer, index, value)`.
    fn atomic_add(&mut self, args: &[CheckedExprId], result: &Type) -> Id {
        let (Some(&buffer), Some(&index), Some(&value)) = (args.first(), args.get(1), args.get(2))
        else {
            return self.undefined(result);
        };
        let Some(base) = self.place(buffer) else {
            return self.undefined(result);
        };
        let at = self.value(index);
        let element = match &base.ty {
            Type::RuntimeArray(element) => element.as_ref().clone(),
            other => other.clone(),
        };
        let target = self.step(&base, at, element);
        let value = self.value(value);
        let result_id = self.ty(result);
        let device = self.uint(scope::DEVICE);
        let relaxed = self.uint(scope::RELAXED);
        let out = self.builder.fresh();
        self.builder.code(
            op::ATOMIC_I_ADD,
            &[
                result_id.word(),
                out.word(),
                target.pointer.word(),
                device.word(),
                relaxed.word(),
                value.word(),
            ],
        );
        out
    }

    /// The image type behind a texture argument.
    fn handle_type(&mut self, texture: CheckedExprId) -> Id {
        let ty = self.module.expr(texture).ty.clone();
        self.ty(&ty)
    }

    /// Every builtin that is one `GLSL.std.450` instruction.
    fn extended(&mut self, which: BuiltinFn, args: &[CheckedExprId], result: &Type) -> Id {
        let operand_ty = args
            .first()
            .map(|&arg| self.module.expr(arg).ty.clone())
            .unwrap_or(Type::Void);
        let scalar = element_scalar(&operand_ty).unwrap_or(ScalarType::Float);
        let values: Vec<Id> = args.iter().map(|&arg| self.value(arg)).collect();
        let Some(instruction) = extended_instruction(which, scalar) else {
            return self.undefined(result);
        };
        // `dot` is a core instruction rather than an extended one, and so is
        // nothing else here — this is the one that had to be special-cased.
        if which == BuiltinFn::Dot {
            let result_id = self.ty(result);
            let out = self.builder.fresh();
            let mut operands = vec![result_id.word(), out.word()];
            operands.extend(values.iter().map(|id| id.word()));
            self.builder.code(op::DOT, &operands);
            return out;
        }
        let result_id = self.ty(result);
        let set = self.builder.glsl;
        let out = self.builder.fresh();
        let mut operands = vec![result_id.word(), out.word(), set.word(), instruction];
        operands.extend(values.iter().map(|id| id.word()));
        self.builder.code(op::EXT_INST, &operands);
        out
    }
}
