//! Lowering statements and expressions into SPIR-V instructions.
//!
//! Every local is an `OpVariable` in the function storage class, loaded and
//! stored through rather than kept in an SSA value. KSL rebinds a `let`
//! constantly, so the alternative is building phi nodes for every branch — and
//! a driver's optimizer promotes these back to registers anyway. The one rule
//! this buys has to be respected: those variables are declared in the function's
//! *first* block, so they are collected before any of the body is walked.
//!
//! Control flow is emitted in SPIR-V's structured form. Every `if` names a merge
//! block before it branches and every loop names a merge and a continue block,
//! because a module whose branches do not say where they rejoin is not a valid
//! module — a shader compiler is entitled to reject it rather than work it out.

mod expr;
mod ops;

use kira_ksl_semantics::model::{
    CheckedExprId, CheckedExprKind, CheckedStmt, CheckedStmtId, ConstValue, UnaryOp,
};
use kira_shader_model::{ScalarType, Type};

use crate::builder::Id;
use crate::lower::ops::{element_scalar, low_word};
use crate::spec::{op, storage_class};
use crate::{Emitter, Global, Place};

impl Emitter<'_> {
    /// Lowers a sequence of statements in a scope of their own.
    pub(crate) fn block(&mut self, body: &[CheckedStmtId]) {
        self.scopes.push(std::collections::HashMap::new());
        for &id in body {
            if self.terminated {
                break;
            }
            self.stmt(id);
        }
        self.scopes.pop();
    }

    /// Lowers one statement.
    fn stmt(&mut self, id: CheckedStmtId) {
        match self.module.stmt(id).clone() {
            CheckedStmt::Let { name, ty, init } => {
                // Every `let` was given a variable by the pre-pass over this
                // same body, so the lookup is total for anything reached here.
                let Some(&pointer) = self.declared.get(&id) else {
                    return;
                };
                if let Some(value) = init {
                    let value = self.value(value);
                    self.builder
                        .code(op::STORE, &[pointer.word(), value.word()]);
                }
                if let Some(top) = self.scopes.last_mut() {
                    top.insert(
                        name,
                        Place {
                            pointer,
                            ty,
                            storage: storage_class::FUNCTION,
                        },
                    );
                }
            }
            CheckedStmt::Assign { target, value } => {
                let Some(place) = self.place(target) else {
                    return;
                };
                let value = self.value(value);
                self.builder
                    .code(op::STORE, &[place.pointer.word(), value.word()]);
            }
            CheckedStmt::If {
                cond,
                then,
                otherwise,
            } => self.branch(cond, &then, otherwise.as_deref()),
            CheckedStmt::While { cond, body } => self.loop_(cond, &body),
            CheckedStmt::Return(None) => {
                self.finish_entry_outputs(None);
                self.builder.code(op::RETURN, &[]);
                self.terminated = true;
            }
            CheckedStmt::Return(Some(value)) => {
                let value = self.value(value);
                if self.entry_outputs.is_some() {
                    self.finish_entry_outputs(Some(value));
                    self.builder.code(op::RETURN, &[]);
                } else {
                    self.builder.code(op::RETURN_VALUE, &[value.word()]);
                }
                self.terminated = true;
            }
            CheckedStmt::Expr(value) => {
                self.value(value);
            }
        }
    }

    /// Copies a returned interface value out to the stage's output variables.
    ///
    /// An entry point returns nothing in SPIR-V — a stage's outputs are
    /// variables the shader writes — so the value the KSL body returns is taken
    /// apart here, one member per output variable.
    fn finish_entry_outputs(&mut self, value: Option<Id>) {
        let Some(outputs) = self.entry_outputs.clone() else {
            return;
        };
        let Some(value) = value else {
            return;
        };
        for (at, (member_ty, variable)) in outputs.iter().enumerate() {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            let extracted = self.builder.fresh();
            self.builder.code(
                op::COMPOSITE_EXTRACT,
                &[member_ty.word(), extracted.word(), value.word(), at],
            );
            self.builder
                .code(op::STORE, &[variable.word(), extracted.word()]);
        }
    }

    /// Emits a structured `if`.
    fn branch(
        &mut self,
        cond: CheckedExprId,
        then: &[CheckedStmtId],
        otherwise: Option<&[CheckedStmtId]>,
    ) {
        let cond = self.value(cond);
        let then_label = self.builder.fresh();
        let else_label = self.builder.fresh();
        let merge_label = self.builder.fresh();
        let target = if otherwise.is_some() {
            else_label
        } else {
            merge_label
        };
        // The merge block is named before the branch, which is what makes this
        // structured control flow rather than a jump a driver has to reconstruct.
        self.builder
            .code(op::SELECTION_MERGE, &[merge_label.word(), 0]);
        self.builder.code(
            op::BRANCH_CONDITIONAL,
            &[cond.word(), then_label.word(), target.word()],
        );

        self.label(then_label);
        self.block(then);
        self.branch_to(merge_label);

        if let Some(otherwise) = otherwise {
            self.label(else_label);
            self.block(otherwise);
            self.branch_to(merge_label);
        }
        self.label(merge_label);
    }

    /// Emits a structured `while`.
    fn loop_(&mut self, cond: CheckedExprId, body: &[CheckedStmtId]) {
        let header = self.builder.fresh();
        let test = self.builder.fresh();
        let body_label = self.builder.fresh();
        let continue_label = self.builder.fresh();
        let merge_label = self.builder.fresh();

        self.branch_to(header);
        self.label(header);
        self.builder.code(
            op::LOOP_MERGE,
            &[merge_label.word(), continue_label.word(), 0],
        );
        self.builder.code(op::BRANCH, &[test.word()]);

        // The condition lives inside the loop: it is re-evaluated every
        // iteration, so it cannot sit in the block before the header.
        self.label(test);
        let cond = self.value(cond);
        self.builder.code(
            op::BRANCH_CONDITIONAL,
            &[cond.word(), body_label.word(), merge_label.word()],
        );

        self.label(body_label);
        self.block(body);
        self.branch_to(continue_label);

        self.label(continue_label);
        self.branch_to(header);
        self.label(merge_label);
    }

    /// Opens a block.
    fn label(&mut self, id: Id) {
        self.builder.code(op::LABEL, &[id.word()]);
        self.terminated = false;
    }

    /// Closes the current block with a branch, unless it already ended.
    fn branch_to(&mut self, id: Id) {
        if !self.terminated {
            self.builder.code(op::BRANCH, &[id.word()]);
        }
        self.terminated = true;
    }

    /// The pointer an assignable expression names.
    pub(crate) fn place(&mut self, id: CheckedExprId) -> Option<Place> {
        let node = self.module.expr(id).clone();
        match &node.kind {
            CheckedExprKind::Local(name) => self.lookup(name),
            CheckedExprKind::Resource(name) => match self.globals.get(name)?.clone() {
                Global::Uniform { pointer, name } => Some(Place {
                    pointer,
                    ty: Type::StructRef(name),
                    storage: storage_class::UNIFORM,
                }),
                // The variable points at the block, and the array is its one
                // member — so every storage access starts one step in.
                Global::Storage {
                    pointer, element, ..
                } => {
                    let array = Type::RuntimeArray(Box::new(element.clone()));
                    // The array the block holds is one of *laid-out* elements,
                    // and an access chain's result type has to be that exact
                    // id: an array of the plain form is a different type with
                    // the same shape, which a validator rejects rather than
                    // quietly indexing.
                    let element_ty = self.laid_out_ty(&element);
                    let array_ty = self.runtime_array(element_ty);
                    let pointer_ty = self.pointer(storage_class::STORAGE_BUFFER, array_ty);
                    let zero = self.uint(0);
                    let stepped = self.builder.fresh();
                    self.builder.code(
                        op::ACCESS_CHAIN,
                        &[
                            pointer_ty.word(),
                            stepped.word(),
                            pointer.word(),
                            zero.word(),
                        ],
                    );
                    Some(Place {
                        pointer: stepped,
                        ty: array,
                        storage: storage_class::STORAGE_BUFFER,
                    })
                }
                Global::Handle { pointer, ty } => Some(Place {
                    pointer,
                    ty,
                    storage: storage_class::UNIFORM_CONSTANT,
                }),
            },
            CheckedExprKind::Field { base, field } => {
                let base = self.place(*base)?;
                let index = self.field_index(&base.ty, field)?;
                let member = self.member_type(&base.ty, field)?;
                let at = self.uint(index);
                Some(self.step(&base, at, member))
            }
            CheckedExprKind::Index { base, index } => {
                let base = self.place(*base)?;
                let at = self.value(*index);
                let element = match &base.ty {
                    Type::RuntimeArray(element) => element.as_ref().clone(),
                    other => other.clone(),
                };
                Some(self.step(&base, at, element))
            }
            // A one-lane swizzle is an address; a wider one names several lanes
            // at once and has no single pointer, which is why an assignment to
            // one never reaches here.
            CheckedExprKind::Swizzle { base, components } if components.len() == 1 => {
                let base = self.place(*base)?;
                let lane = u32::from(*components.first()?);
                let at = self.uint(lane);
                let element = match &base.ty {
                    Type::Vector(vector) => Type::Scalar(vector.scalar),
                    other => other.clone(),
                };
                Some(self.step(&base, at, element))
            }
            _ => None,
        }
    }

    /// One `OpAccessChain` step from `base` by `index`.
    pub(crate) fn step(&mut self, base: &Place, index: Id, member: Type) -> Place {
        let member_ty = if base.storage == storage_class::UNIFORM
            || base.storage == storage_class::STORAGE_BUFFER
        {
            self.laid_out_ty(&member)
        } else {
            self.ty(&member)
        };
        let pointer_ty = self.pointer(base.storage, member_ty);
        let stepped = self.builder.fresh();
        self.builder.code(
            op::ACCESS_CHAIN,
            &[
                pointer_ty.word(),
                stepped.word(),
                base.pointer.word(),
                index.word(),
            ],
        );
        Place {
            pointer: stepped,
            ty: member,
            storage: base.storage,
        }
    }

    /// The type a member takes inside a buffer, which is the laid-out form for
    /// a struct and the ordinary form for everything else.
    pub(crate) fn laid_out_ty(&mut self, ty: &Type) -> Id {
        match ty {
            Type::StructRef(name) => self.laid_out_struct(name),
            other => self.ty(other),
        }
    }

    /// The local `name` names, innermost scope first.
    fn lookup(&self, name: &str) -> Option<Place> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name))
            .cloned()
    }

    /// The index of `field` in the struct `ty` names.
    fn field_index(&self, ty: &Type, field: &str) -> Option<u32> {
        let Type::StructRef(name) = ty else {
            return None;
        };
        let declared = self.module.struct_named(name)?;
        let at = declared
            .fields
            .iter()
            .position(|candidate| candidate.name == field)?;
        u32::try_from(at).ok()
    }

    /// The type of `field` in the struct `ty` names.
    fn member_type(&self, ty: &Type, field: &str) -> Option<Type> {
        let Type::StructRef(name) = ty else {
            return None;
        };
        let declared = self.module.struct_named(name)?;
        declared
            .fields
            .iter()
            .find(|candidate| candidate.name == field)
            .map(|found| found.ty.clone())
    }

    /// Loads whatever `place` points at.
    pub(crate) fn load(&mut self, place: &Place) -> Id {
        let ty = if place.storage == storage_class::UNIFORM
            || place.storage == storage_class::STORAGE_BUFFER
        {
            self.laid_out_ty(&place.ty)
        } else {
            self.ty(&place.ty)
        };
        let loaded = self.builder.fresh();
        self.builder
            .code(op::LOAD, &[ty.word(), loaded.word(), place.pointer.word()]);
        loaded
    }

    /// The value an expression produces.
    pub(crate) fn value(&mut self, id: CheckedExprId) -> Id {
        let node = self.module.expr(id).clone();
        match &node.kind {
            CheckedExprKind::Const(value) => self.constant(*value),
            CheckedExprKind::Option(name) => {
                let value = self
                    .options
                    .get(name)
                    .copied()
                    .unwrap_or(ConstValue::Uint(0));
                self.constant(value)
            }
            CheckedExprKind::Local(_)
            | CheckedExprKind::Resource(_)
            | CheckedExprKind::Field { .. }
            | CheckedExprKind::Index { .. } => match self.place(id) {
                Some(place) => {
                    let loaded = self.load(&place);
                    self.rebuild_plain(loaded, &place)
                }
                None => self.undefined(&node.ty),
            },
            CheckedExprKind::Swizzle { base, components } => {
                let base_value = self.value(*base);
                let result = self.ty(&node.ty);
                let out = self.builder.fresh();
                if components.len() == 1 {
                    let lane = u32::from(components[0]);
                    self.builder.code(
                        op::COMPOSITE_EXTRACT,
                        &[result.word(), out.word(), base_value.word(), lane],
                    );
                } else {
                    let mut operands = vec![
                        result.word(),
                        out.word(),
                        base_value.word(),
                        base_value.word(),
                    ];
                    operands.extend(components.iter().map(|&lane| u32::from(lane)));
                    self.builder.code(op::VECTOR_SHUFFLE, &operands);
                }
                out
            }
            CheckedExprKind::ArrayLength { base } => self.array_length(*base),
            CheckedExprKind::Construct { args } => {
                let values: Vec<Id> = args.iter().map(|&arg| self.value(arg)).collect();
                let result = self.ty(&node.ty);
                let out = self.builder.fresh();
                let mut operands = vec![result.word(), out.word()];
                operands.extend(values.iter().map(|id| id.word()));
                self.builder.code(op::COMPOSITE_CONSTRUCT, &operands);
                out
            }
            CheckedExprKind::Cast { value } => {
                let source = self.module.expr(*value).ty.clone();
                let value = self.value(*value);
                self.convert(value, &source, &node.ty)
            }
            CheckedExprKind::Call { name, args } => {
                let values: Vec<Id> = args.iter().map(|&arg| self.value(arg)).collect();
                let Some(&callee) = self.callable.get(name) else {
                    return self.undefined(&node.ty);
                };
                let result = self.ty(&node.ty);
                let out = self.builder.fresh();
                let mut operands = vec![result.word(), out.word(), callee.word()];
                operands.extend(values.iter().map(|id| id.word()));
                self.builder.code(op::FUNCTION_CALL, &operands);
                out
            }
            CheckedExprKind::Builtin { which, args } => self.builtin(*which, args, &node.ty),
            CheckedExprKind::Unary { op, operand } => {
                let operand_ty = self.module.expr(*operand).ty.clone();
                let operand = self.value(*operand);
                let result = self.ty(&node.ty);
                let opcode = match (op, element_scalar(&operand_ty)) {
                    (UnaryOp::Neg, Some(ScalarType::Float)) => op::F_NEGATE,
                    (UnaryOp::Neg, _) => op::S_NEGATE,
                    (UnaryOp::Not, _) => op::LOGICAL_NOT,
                };
                let out = self.builder.fresh();
                self.builder
                    .code(opcode, &[result.word(), out.word(), operand.word()]);
                out
            }
            CheckedExprKind::Binary { op, lhs, rhs } => self.binary(*op, *lhs, *rhs, &node.ty),
            CheckedExprKind::Invalid => self.undefined(&node.ty),
        }
    }

    /// A value read out of a buffer, put back into its undecorated form.
    ///
    /// A struct inside a buffer is a different SPIR-V type from the same struct
    /// as a local — one carries offsets and the other may not — so a whole
    /// struct read out of a uniform is taken apart and rebuilt rather than
    /// handed over with the buffer's type still on it.
    fn rebuild_plain(&mut self, value: Id, place: &Place) -> Id {
        let Type::StructRef(name) = &place.ty else {
            return value;
        };
        if place.storage != storage_class::UNIFORM && place.storage != storage_class::STORAGE_BUFFER
        {
            return value;
        }
        let Some(declared) = self.module.struct_named(name).cloned() else {
            return value;
        };
        let mut members = Vec::with_capacity(declared.fields.len());
        for (at, field) in declared.fields.iter().enumerate() {
            let at = u32::try_from(at).unwrap_or(u32::MAX);
            let member_ty = self.laid_out_ty(&field.ty);
            let extracted = self.builder.fresh();
            self.builder.code(
                op::COMPOSITE_EXTRACT,
                &[member_ty.word(), extracted.word(), value.word(), at],
            );
            let inner = Place {
                pointer: extracted,
                ty: field.ty.clone(),
                storage: place.storage,
            };
            members.push(self.rebuild_plain(extracted, &inner));
        }
        let plain = self.plain_struct(name);
        let out = self.builder.fresh();
        let mut operands = vec![plain.word(), out.word()];
        operands.extend(members.iter().map(|id| id.word()));
        self.builder.code(op::COMPOSITE_CONSTRUCT, &operands);
        out
    }

    /// A storage buffer's element count.
    fn array_length(&mut self, base: CheckedExprId) -> Id {
        let uint = self.int(false);
        let CheckedExprKind::Resource(name) = &self.module.expr(base).kind.clone() else {
            return self.builder.constant(uint, 0);
        };
        let Some(Global::Storage { pointer, .. }) = self.globals.get(name).cloned() else {
            return self.builder.constant(uint, 0);
        };
        let out = self.builder.fresh();
        // Asked of the block variable and its one member, never of the array
        // pointer: the length is the binding's, not the type's.
        self.builder.code(
            op::ARRAY_LENGTH,
            &[uint.word(), out.word(), pointer.word(), 0],
        );
        out
    }

    /// A constant of the type its value carries.
    fn constant(&mut self, value: ConstValue) -> Id {
        match value {
            ConstValue::Bool(value) => {
                let ty = self.bool();
                self.builder.constant_bool(ty, value)
            }
            // A shader's integers are 32 bits wide; the checked model carries
            // them in a 64-bit slot, so the constant is the low word of the
            // pattern rather than a value that could fail to fit.
            ConstValue::Int(value) => {
                let ty = self.int(true);
                let bits = low_word(u64::from_ne_bytes(value.to_ne_bytes()));
                self.builder.constant(ty, bits)
            }
            ConstValue::Uint(value) => {
                let ty = self.int(false);
                let bits = low_word(value);
                self.builder.constant(ty, bits)
            }
            ConstValue::Float(value) => {
                let ty = self.float();
                // Likewise 32 bits wide: the double is the parser's carrier,
                // and the constant a driver reads is the single it rounds to.
                let bits = (value as f32).to_bits();
                self.builder.constant(ty, bits)
            }
        }
    }

    /// A zero of `ty`, for the paths a checked module cannot actually reach.
    ///
    /// Emission is total: a module that failed checking never gets here, and
    /// one that passed has no invalid expression in it — but a backend that
    /// answered `None` would make every caller handle a case that cannot
    /// happen.
    pub(crate) fn undefined(&mut self, ty: &Type) -> Id {
        let id = self.ty(ty);
        self.builder.constant(id, 0)
    }
}
