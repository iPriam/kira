//! Path-addressed mutation of backend-neutral callback state.

use kira_ir::{IrExpr, IrExprId, IrPlace, IrPlaceStep};
use kira_runtime_abi::{NativeStatePathStep, NativeStateTypeId};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::lower::FunctionLowering;
use crate::LlvmError;

struct LoweredStatePath {
    kinds: LLVMValueRef,
    values: LLVMValueRef,
    count: LLVMValueRef,
    target: Type,
    kinds_saved: Option<LLVMValueRef>,
    values_saved: Option<LLVMValueRef>,
}

impl FunctionLowering<'_, '_> {
    /// Reads one field chain rooted at a recovered callback-state local directly
    /// from the backend-neutral tree. This avoids materializing an owning native
    /// snapshot merely to reach a leaf, which would otherwise manufacture
    /// temporary `Drop` values whose bodies do not belong to the read.
    pub(super) fn lower_native_state_field_read(
        &mut self,
        base: IrExprId,
        field: u32,
        ty: Type,
    ) -> Result<Option<LLVMValueRef>, LlvmError> {
        if self.state_is_boxed() {
            return Ok(None);
        }
        let mut steps = vec![IrPlaceStep::Field(field)];
        let mut cursor = base;
        loop {
            let (next, step) = match self.codegen.program.expr(cursor) {
                IrExpr::Field { base, index, .. } => {
                    (Some(*base), Some(IrPlaceStep::Field(*index)))
                }
                IrExpr::Index { base, index, .. } => {
                    (Some(*base), Some(IrPlaceStep::Index(*index)))
                }
                IrExpr::Local(slot) => {
                    let slot = *slot;
                    let Some(type_id) = self
                        .function
                        .native_state_locals
                        .get(slot as usize)
                        .copied()
                        .flatten()
                    else {
                        return Ok(None);
                    };
                    steps.reverse();
                    let root_ty = self.local_type(slot)?;
                    let path = self.lower_native_state_path(root_ty, &steps)?;
                    if path.target != ty {
                        return Err(LlvmError::internal(
                            "a callback-state read path resolved to the wrong type",
                        ));
                    }
                    let token = self.native_state_token_of_local(slot)?;
                    let out = self.alloca(self.codegen.types.ptr, c"native.state.path.read.node");
                    let status = self.call(
                        self.codegen.runtime.native_state_read_path,
                        &mut [
                            token,
                            self.codegen.const_int(type_id.as_word() as i64),
                            path.kinds,
                            path.values,
                            path.count,
                            out,
                        ],
                        c"native.state.status",
                    );
                    self.check_native_state_status(status);
                    // SAFETY: a successful read initializes one owned node pointer.
                    let node = unsafe {
                        LLVMBuildLoad2(
                            self.codegen.builder,
                            self.codegen.types.ptr,
                            out,
                            c"native.state.path.read.node".as_ptr(),
                        )
                    };
                    let value = self.codegen.decode_native_state_value(node, ty)?;
                    self.release_native_state_path(path);
                    return Ok(Some(value));
                }
                _ => return Ok(None),
            };
            if let Some(step) = step {
                steps.push(step);
            }
            let Some(next) = next else {
                return Ok(None);
            };
            cursor = next;
        }
    }

    /// Stores one expression through a recovered callback-state place without
    /// materializing and replacing the whole state tree.
    pub(super) fn store_native_state_place(
        &mut self,
        place: &IrPlace,
        type_id: NativeStateTypeId,
        root_ty: Type,
        expr: IrExprId,
    ) -> Result<(), LlvmError> {
        let path = self.lower_native_state_path(root_ty, &place.path)?;
        let lowered = self.lower_expr(expr)?;
        let lowered = self.prepare_store_value(path.target, expr, lowered);
        let node = self
            .codegen
            .encode_native_state_value(lowered, path.target)?;
        let token = self.native_state_token_of_local(place.local)?;
        let old_out = self.alloca(self.codegen.types.ptr, c"native.state.path.old.node");
        let status = self.call(
            self.codegen.runtime.native_state_write_path,
            &mut [
                token,
                self.codegen.const_int(type_id.as_word() as i64),
                path.kinds,
                path.values,
                path.count,
                node,
                old_out,
            ],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        // SAFETY: a successful path replacement initializes one displaced node.
        let old_node = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                old_out,
                c"native.state.path.old.node".as_ptr(),
            )
        };
        let old = self
            .codegen
            .decode_native_state_value(old_node, path.target)?;
        self.drop_value(old, path.target)?;
        self.release_native_state_path(path);
        Ok(())
    }

    /// Appends one expression to the path-addressed array in callback state.
    pub(super) fn append_native_state_place(
        &mut self,
        place: &IrPlace,
        type_id: NativeStateTypeId,
        root_ty: Type,
        expr: IrExprId,
    ) -> Result<(), LlvmError> {
        let path = self.lower_native_state_path(root_ty, &place.path)?;
        let element = self.codegen.element_of(path.target)?;
        let value = self.lower_expr(expr)?;
        let node = self.codegen.encode_native_state_value(value, element)?;
        let token = self.native_state_token_of_local(place.local)?;
        let status = self.call(
            self.codegen.runtime.native_state_append_path,
            &mut [
                token,
                self.codegen.const_int(type_id.as_word() as i64),
                path.kinds,
                path.values,
                path.count,
                node,
            ],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        self.release_native_state_path(path);
        Ok(())
    }

    /// Lowers dynamic indices left-to-right and writes a compact parallel-array
    /// representation of the path for the native runtime.
    fn lower_native_state_path(
        &mut self,
        root_ty: Type,
        steps: &[IrPlaceStep],
    ) -> Result<LoweredStatePath, LlvmError> {
        if steps.is_empty() {
            // SAFETY: `types.ptr` is this context's opaque pointer type; a null
            // constant of it stands in for the two empty parallel arrays.
            let null_ptr = unsafe { LLVMConstNull(self.codegen.types.ptr) };
            return Ok(LoweredStatePath {
                kinds: null_ptr,
                values: null_ptr,
                count: self.codegen.const_int(0),
                target: root_ty,
                kinds_saved: None,
                values_saved: None,
            });
        }
        let (kinds, kinds_saved) = self.codegen.dynamic_array_alloca(
            self.codegen.types.i8,
            steps.len() as u64,
            c"native.state.path.kinds",
        );
        let (values, values_saved) = self.codegen.dynamic_array_alloca(
            self.codegen.types.i64,
            steps.len() as u64,
            c"native.state.path.values",
        );
        let mut ty = root_ty;
        for (slot, step) in steps.iter().enumerate() {
            let (kind, value, next_ty) =
                match step {
                    IrPlaceStep::Field(index) => {
                        let Type::Struct(id) = ty else {
                            return Err(LlvmError::internal(
                                "a callback-state field path stepped through a non-struct",
                            ));
                        };
                        let def = self.codegen.program.types.structs().get(id).ok_or(
                            LlvmError::internal("a callback-state path names a missing struct"),
                        )?;
                        let next = if def.owns_c_storage_at(*index) {
                            Type::CBlock
                        } else {
                            def.field(*index)
                                .map(|field| field.ty)
                                .ok_or(LlvmError::internal(
                                    "a callback-state path names a missing field",
                                ))?
                        };
                        (
                            NativeStatePathStep::FIELD_WIRE_TAG,
                            self.codegen.const_int(i64::from(*index)),
                            next,
                        )
                    }
                    IrPlaceStep::Index(index) => {
                        let next = self.codegen.element_of(ty)?;
                        let value = self.lower_expr(*index)?;
                        (NativeStatePathStep::INDEX_WIRE_TAG, value, next)
                    }
                };
            // SAFETY: both arrays were allocated for exactly `steps.len()`
            // elements, and `slot` is inside that range.
            unsafe {
                let mut offset = [LLVMConstInt(self.codegen.types.i32, slot as u64, 0)];
                let kind_at = LLVMBuildInBoundsGEP2(
                    self.codegen.builder,
                    self.codegen.types.i8,
                    kinds,
                    offset.as_mut_ptr(),
                    1,
                    c"native.state.path.kind".as_ptr(),
                );
                LLVMBuildStore(
                    self.codegen.builder,
                    LLVMConstInt(self.codegen.types.i8, u64::from(kind), 0),
                    kind_at,
                );
                let value_at = LLVMBuildInBoundsGEP2(
                    self.codegen.builder,
                    self.codegen.types.i64,
                    values,
                    offset.as_mut_ptr(),
                    1,
                    c"native.state.path.value".as_ptr(),
                );
                LLVMBuildStore(self.codegen.builder, value, value_at);
            }
            ty = next_ty;
        }
        Ok(LoweredStatePath {
            kinds,
            values,
            count: self.codegen.const_int(steps.len() as i64),
            target: ty,
            kinds_saved: Some(kinds_saved),
            values_saved: Some(values_saved),
        })
    }

    fn release_native_state_path(&mut self, path: LoweredStatePath) {
        if let Some(saved) = path.values_saved {
            self.codegen.release_dynamic_alloca(path.values, saved);
        }
        if let Some(saved) = path.kinds_saved {
            self.codegen.release_dynamic_alloca(path.kinds, saved);
        }
    }
}
