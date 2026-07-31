//! Emitting functions, and the entry point that marshals a stage's interface.
//!
//! A SPIR-V entry point returns nothing and takes nothing, so the struct a KSL
//! stage was written against is built here from the input variables on the way
//! in and taken apart into the output variables on the way out. Every
//! function-scope variable is declared in the first block, which is why the
//! `let`s are collected before any of the body is walked.

use std::collections::HashMap;

use kira_ksl_semantics::model::{CheckedFunction, CheckedStage, CheckedStmt, CheckedStmtId};
use kira_shader_model::{ReflectedStage, ScalarType, Type};

use crate::builder::Id;
use crate::spec::{op, storage_class};
use crate::{Emitter, Interface, Place};

impl Emitter<'_> {
    /// Emits one ordinary function.
    pub(crate) fn function(&mut self, function: &CheckedFunction, id: Id) {
        let result = self.ty(&function.result);
        let params: Vec<Id> = function
            .params
            .iter()
            .map(|param| self.ty(&param.ty))
            .collect();
        let signature = self.function_type(result, &params);
        self.builder.code(
            op::FUNCTION,
            &[result.word(), id.word(), 0, signature.word()],
        );
        let param_ids: Vec<Id> = params
            .iter()
            .map(|&ty| {
                let param = self.builder.fresh();
                self.builder
                    .code(op::FUNCTION_PARAMETER, &[ty.word(), param.word()]);
                param
            })
            .collect();

        let label = self.builder.fresh();
        self.builder.code(op::LABEL, &[label.word()]);
        self.terminated = false;
        self.scopes.push(HashMap::new());

        // A parameter is a value in SPIR-V and a place in KSL, so each one gets
        // a variable of its own to be written through.
        let mut copies = Vec::new();
        for (param, &value) in function.params.iter().zip(&param_ids) {
            let pointer = self.variable(&param.ty);
            self.builder.name(pointer, &param.name);
            if let Some(top) = self.scopes.last_mut() {
                top.insert(
                    param.name.clone(),
                    Place {
                        pointer,
                        ty: param.ty.clone(),
                        storage: storage_class::FUNCTION,
                    },
                );
            }
            copies.push((pointer, value));
        }
        self.declare_locals(&function.body);
        for (pointer, value) in copies {
            self.builder
                .code(op::STORE, &[pointer.word(), value.word()]);
        }

        self.block(&function.body);
        if !self.terminated {
            // A body whose last statement is not a `return` still has to end
            // its block; a void function is the only shape that reaches here.
            self.builder.code(op::RETURN, &[]);
        }
        self.scopes.pop();
        self.builder.code(op::FUNCTION_END, &[]);
    }

    /// Emits the stage's entry point, marshalling its interface at both ends.
    pub(crate) fn entry(
        &mut self,
        id: Id,
        checked: &CheckedStage,
        reflected: &ReflectedStage,
        inputs: &[Interface],
        outputs: &[Interface],
    ) {
        let void = self.void();
        let signature = self.function_type(void, &[]);
        self.builder
            .code(op::FUNCTION, &[void.word(), id.word(), 0, signature.word()]);
        let label = self.builder.fresh();
        self.builder.code(op::LABEL, &[label.word()]);
        self.terminated = false;
        self.scopes.push(HashMap::new());

        let param = checked.entry.params.first().cloned();
        let input_variable = param.as_ref().map(|param| {
            let pointer = self.variable(&param.ty);
            self.builder.name(pointer, &param.name);
            pointer
        });
        self.declare_locals(&checked.entry.body);

        // On the way in: read every input variable and rebuild the struct the
        // body was written against.
        if let (Some(param), Some(pointer)) = (param.as_ref(), input_variable) {
            let loaded: Vec<(String, Id)> = inputs
                .iter()
                .map(|(ty, variable, name)| {
                    let ty = self.ty(ty);
                    let out = self.builder.fresh();
                    self.builder
                        .code(op::LOAD, &[ty.word(), out.word(), variable.word()]);
                    (name.clone(), out)
                })
                .collect();
            let composed = self.compose(&param.ty, &loaded);
            self.builder
                .code(op::STORE, &[pointer.word(), composed.word()]);
            if let Some(top) = self.scopes.last_mut() {
                top.insert(
                    param.name.clone(),
                    Place {
                        pointer,
                        ty: param.ty.clone(),
                        storage: storage_class::FUNCTION,
                    },
                );
            }
        }

        self.entry_outputs = Some(
            outputs
                .iter()
                .map(|(ty, variable, _)| {
                    let ty = self.ty(ty);
                    (ty, *variable)
                })
                .collect(),
        );
        self.block(&checked.entry.body);
        if !self.terminated {
            self.builder.code(op::RETURN, &[]);
        }
        self.entry_outputs = None;
        self.scopes.pop();
        self.builder.code(op::FUNCTION_END, &[]);
        let _ = reflected;
    }

    /// Builds a struct value of `ty` from the members already loaded.
    ///
    /// A compute stage's input struct holds members no variable was made for —
    /// its non-builtin fields — so the missing ones are zeroes rather than a
    /// shorter composite, which would not be a value of that type at all.
    fn compose(&mut self, ty: &Type, loaded: &[(String, Id)]) -> Id {
        let result = self.ty(ty);
        let Type::StructRef(name) = ty else {
            let out = self.builder.fresh();
            let mut operands = vec![result.word(), out.word()];
            operands.extend(loaded.iter().map(|(_, id)| id.word()));
            self.builder.code(op::COMPOSITE_CONSTRUCT, &operands);
            return out;
        };
        let fields = self
            .module
            .struct_named(name)
            .map(|declared| declared.fields.clone())
            .unwrap_or_default();
        let members: Vec<Id> = fields
            .iter()
            .map(|field| {
                match loaded
                    .iter()
                    .find(|(member, _)| *member == field.name)
                    .map(|(_, id)| *id)
                {
                    Some(id) => id,
                    None => self.zero(&field.ty),
                }
            })
            .collect();
        let out = self.builder.fresh();
        let mut operands = vec![result.word(), out.word()];
        operands.extend(members.iter().map(|id| id.word()));
        self.builder.code(op::COMPOSITE_CONSTRUCT, &operands);
        out
    }

    /// A zero value of `ty`, built rather than left undefined.
    fn zero(&mut self, ty: &Type) -> Id {
        match ty {
            Type::Scalar(ScalarType::Bool) => {
                let id = self.bool();
                self.builder.constant_bool(id, false)
            }
            Type::Scalar(ScalarType::Float) => {
                let id = self.float();
                self.builder.constant(id, 0)
            }
            Type::Scalar(ScalarType::Int) => self.sint(0),
            Type::Scalar(ScalarType::Uint) => self.uint(0),
            other => {
                let id = self.ty(other);
                self.builder.constant(id, 0)
            }
        }
    }

    /// A function-scope variable of `ty`.
    fn variable(&mut self, ty: &Type) -> Id {
        let pointee = self.ty(ty);
        let pointer_ty = self.pointer(storage_class::FUNCTION, pointee);
        let variable = self.builder.fresh();
        self.builder.code(
            op::VARIABLE,
            &[pointer_ty.word(), variable.word(), storage_class::FUNCTION],
        );
        variable
    }

    /// Gives every `let` in `body` its variable, nested blocks included.
    ///
    /// All of them at once and before the body is walked, because SPIR-V puts
    /// every function-scope variable in the function's first block.
    fn declare_locals(&mut self, body: &[CheckedStmtId]) {
        for (id, ty) in self.lets(body) {
            let pointer = self.variable(&ty);
            self.declared.insert(id, pointer);
        }
    }

    /// Every `let` in `body` and what it holds.
    fn lets(&self, body: &[CheckedStmtId]) -> Vec<(CheckedStmtId, Type)> {
        let mut found = Vec::new();
        for &id in body {
            match self.module.stmt(id) {
                CheckedStmt::Let { ty, .. } => found.push((id, ty.clone())),
                CheckedStmt::If {
                    then, otherwise, ..
                } => {
                    found.extend(self.lets(then));
                    if let Some(otherwise) = otherwise {
                        found.extend(self.lets(otherwise));
                    }
                }
                CheckedStmt::While { body, .. } => found.extend(self.lets(body)),
                _ => {}
            }
        }
        found
    }
}
