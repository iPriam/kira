//! Generated encode/decode helpers for enum callback-state values.

use kira_runtime_abi::{EnumPayloadKind, NativeStateStatus, NativeStateValueTag};
use kira_semantics_model::{EnumId, Type};
use llvm_sys::LLVMLinkage;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::ffi::c_string;
use super::native_state::StateLeaf;
use super::{Callable, Codegen};
use crate::LlvmError;

impl Codegen<'_> {
    /// Returns a memoized enum state encoder or decoder.
    pub(super) fn native_state_enum_leaf(
        &mut self,
        id: EnumId,
        leaf: StateLeaf,
    ) -> Result<Callable, LlvmError> {
        if let Some(callable) = self.native_state_enum_leaves.get(&(id, leaf)) {
            return Ok(*callable);
        }
        let ordinal = self.native_state_enum_leaves.len();
        let name = c_string(&match leaf {
            StateLeaf::Encode => format!("kira.native.state.enum.encode.{ordinal}"),
            StateLeaf::Decode => format!("kira.native.state.enum.decode.{ordinal}"),
        });
        let result = match leaf {
            StateLeaf::Encode => self.types.ptr,
            StateLeaf::Decode => self.types.ptr,
        };
        let mut params = [self.types.ptr];
        // SAFETY: all types belong to this context and the signature slice lives
        // through the declaration.
        let callable = unsafe {
            let ty = LLVMFunctionType(result, params.as_mut_ptr(), 1, 0);
            let value = LLVMAddFunction(self.module, name.as_ptr(), ty);
            LLVMSetLinkage(value, LLVMLinkage::LLVMInternalLinkage);
            Callable { ty, value }
        };
        self.native_state_enum_leaves.insert((id, leaf), callable);

        // SAFETY: the function is live and receives one fresh entry block.
        let entry = unsafe {
            LLVMAppendBasicBlockInContext(self.context, callable.value, c"entry".as_ptr())
        };
        // SAFETY: save the current builder position around helper emission.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: `entry` belongs to this helper.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };
        let emitted = match leaf {
            StateLeaf::Encode => self.emit_enum_state_encode(id, callable.value),
            StateLeaf::Decode => self.emit_enum_state_decode(id, callable.value),
        };
        // SAFETY: restore the caller's block when one existed.
        unsafe {
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        emitted?;
        Ok(callable)
    }

    fn emit_enum_state_encode(
        &mut self,
        id: EnumId,
        function: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let variants: Vec<Option<Type>> = self
            .program
            .types
            .enums()
            .get(id)
            .map(|def| def.variants.iter().map(|variant| variant.payload).collect())
            .ok_or(LlvmError::internal("an enum the program never declared"))?;
        // SAFETY: the helper has one pointer parameter.
        let value = unsafe { LLVMGetParam(function, 0) };
        let tag = self.call(self.runtime.enum_tag, &mut [value], c"enum.tag");
        let merge = self.append_enum_block(function, c"merge");
        let invalid = self.append_enum_block(function, c"invalid");
        // SAFETY: the builder is on entry and every case constant is i64.
        let switch = unsafe { LLVMBuildSwitch(self.builder, tag, invalid, variants.len() as u32) };
        let mut incoming = Vec::with_capacity(variants.len());
        for (index, payload) in variants.into_iter().enumerate() {
            let block = self.append_enum_block(function, c"variant");
            // SAFETY: switch and block belong to this function.
            unsafe { LLVMAddCase(switch, self.const_int(index as i64), block) };
            // SAFETY: position at this fresh block.
            unsafe { LLVMPositionBuilderAtEnd(self.builder, block) };
            let enum_tag = self.const_i32(index as u32);
            let node = match payload {
                Some(payload_ty) => {
                    let payload = self.read_runtime_enum_payload(value, payload_ty)?;
                    let child = self.encode_native_state_value(payload, payload_ty)?;
                    let node = self.aggregate_node_dynamic(NativeStateValueTag::ENUM, enum_tag, 1);
                    self.set_native_child(node, 0, child);
                    node
                }
                None => self.aggregate_node_dynamic(NativeStateValueTag::ENUM, enum_tag, 0),
            };
            self.call(self.runtime.enum_free, &mut [value], c"");
            // SAFETY: this case is unterminated and joins merge.
            unsafe { LLVMBuildBr(self.builder, merge) };
            // Status checks and nested conversions may split this case; the PHI
            // predecessor is the block that actually branches to `merge`.
            // SAFETY: the builder remains positioned on that live predecessor.
            let predecessor = unsafe { LLVMGetInsertBlock(self.builder) };
            incoming.push((node, predecessor));
        }
        self.emit_invalid_enum_state(invalid);
        // SAFETY: merge belongs to the helper and receives one incoming pointer
        // from each variant block.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, merge) };
        let phi = self.enum_phi(&mut incoming, c"native.enum.node");
        // SAFETY: phi is the helper's pointer result.
        unsafe { LLVMBuildRet(self.builder, phi) };
        Ok(())
    }

    fn emit_enum_state_decode(
        &mut self,
        id: EnumId,
        function: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let variants: Vec<Option<Type>> = self
            .program
            .types
            .enums()
            .get(id)
            .map(|def| def.variants.iter().map(|variant| variant.payload).collect())
            .ok_or(LlvmError::internal("an enum the program never declared"))?;
        // SAFETY: the helper has one node-pointer parameter.
        let node = unsafe { LLVMGetParam(function, 0) };
        let tag32 = self.call(
            self.runtime.native_value_enum_tag,
            &mut [node],
            c"native.enum.tag",
        );
        // SAFETY: zero extension preserves the u32 declaration tag.
        let tag =
            unsafe { LLVMBuildZExt(self.builder, tag32, self.types.i64, c"enum.tag".as_ptr()) };
        let merge = self.append_enum_block(function, c"merge");
        let invalid = self.append_enum_block(function, c"invalid");
        // SAFETY: the builder is on entry and every case constant is i64.
        let switch = unsafe { LLVMBuildSwitch(self.builder, tag, invalid, variants.len() as u32) };
        let mut incoming = Vec::with_capacity(variants.len());
        for (index, payload) in variants.into_iter().enumerate() {
            let block = self.append_enum_block(function, c"variant");
            // SAFETY: switch and block belong to this function.
            unsafe { LLVMAddCase(switch, self.const_int(index as i64), block) };
            // SAFETY: position at this fresh block.
            unsafe { LLVMPositionBuilderAtEnd(self.builder, block) };
            let tag = self.const_int(index as i64);
            let boxed = match payload {
                Some(payload_ty) => {
                    let child = self.call(
                        self.runtime.native_value_child,
                        &mut [node, self.const_int(0)],
                        c"native.enum.payload",
                    );
                    let payload = self.decode_native_state_value(child, payload_ty)?;
                    self.box_runtime_enum_payload(tag, payload_ty, payload)?
                }
                None => self.call(
                    self.runtime.enum_new,
                    &mut [tag, self.const_int(0), self.const_int(0)],
                    c"enum",
                ),
            };
            self.call(self.runtime.native_value_free, &mut [node], c"");
            // SAFETY: this case is unterminated and joins merge.
            unsafe { LLVMBuildBr(self.builder, merge) };
            // Nested decoding may split this case; record the actual branch.
            // SAFETY: the builder remains positioned on that live predecessor.
            let predecessor = unsafe { LLVMGetInsertBlock(self.builder) };
            incoming.push((boxed, predecessor));
        }
        self.emit_invalid_enum_state(invalid);
        // SAFETY: merge belongs to the helper and receives one pointer per case.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, merge) };
        let phi = self.enum_phi(&mut incoming, c"native.enum");
        // SAFETY: phi is the helper's pointer result.
        unsafe { LLVMBuildRet(self.builder, phi) };
        Ok(())
    }

    fn read_runtime_enum_payload(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if matches!(ty, Type::Struct(_) | Type::Array(_)) {
            let llvm_type = self.llvm_type(ty)?;
            // SAFETY: `llvm_type` belongs to this context and the runtime writes
            // one owned aggregate value into this slot.
            let (out, saved) = self.dynamic_alloca(llvm_type, c"enum.aggregate.payload");
            self.lifetime_start(out);
            self.call(self.runtime.enum_payload_aggregate, &mut [value, out], c"");
            // SAFETY: the helper initialized `out` as `llvm_type`.
            let payload = unsafe {
                LLVMBuildLoad2(
                    self.builder,
                    llvm_type,
                    out,
                    c"enum.aggregate.value".as_ptr(),
                )
            };
            self.release_dynamic_alloca(out, saved);
            return Ok(payload);
        }
        let word = self.call(self.runtime.enum_payload, &mut [value], c"enum.payload");
        self.decode_payload_word(ty, word)
    }

    fn box_runtime_enum_payload(
        &mut self,
        tag: LLVMValueRef,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        if matches!(ty, Type::Struct(_) | Type::Array(_)) {
            let llvm_type = self.llvm_type(ty)?;
            let size = self.abi_size(ty)?;
            // SAFETY: the slot belongs to this context and `value` has its type.
            let (source, saved) = self.dynamic_alloca(llvm_type, c"enum.aggregate.source");
            self.lifetime_start(source);
            // SAFETY: `source` was allocated with `llvm_type` and `value` has
            // that same type.
            unsafe { LLVMBuildStore(self.builder, value, source) };
            let clone = self.element_clone(ty)?;
            let free = self.element_free(ty)?;
            let result = self.call(
                self.runtime.enum_new_aggregate,
                &mut [tag, source, size, clone, free],
                c"enum.aggregate",
            );
            self.release_dynamic_alloca(source, saved);
            return Ok(result);
        }
        let (kind, word) = self.encode_payload_word(ty, value)?;
        Ok(self.call(self.runtime.enum_new, &mut [tag, kind, word], c"enum"))
    }

    fn decode_payload_word(
        &mut self,
        ty: Type,
        word: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        // SAFETY: the enum runtime produced this word for the declared payload.
        Ok(unsafe {
            match ty {
                Type::Int(_) | Type::RawPtr | Type::ForeignPtr(_) => word,
                Type::Float(_) => LLVMBuildBitCast(
                    self.builder,
                    word,
                    self.types.f64,
                    c"payload.float".as_ptr(),
                ),
                Type::Bool => {
                    LLVMBuildTrunc(self.builder, word, self.types.i1, c"payload.bool".as_ptr())
                }
                Type::String | Type::Enum(_) | Type::Any | Type::Cell(_) => LLVMBuildIntToPtr(
                    self.builder,
                    word,
                    self.types.ptr,
                    c"payload.handle".as_ptr(),
                ),
                _ => return Err(LlvmError::internal("an unsupported enum payload")),
            }
        })
    }

    fn encode_payload_word(
        &mut self,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(LLVMValueRef, LLVMValueRef), LlvmError> {
        let kind = match ty {
            Type::String => EnumPayloadKind::STR,
            Type::Enum(_) | Type::Any | Type::Cell(_) => EnumPayloadKind::ENUM,
            Type::Int(_) | Type::Float(_) | Type::Bool | Type::RawPtr | Type::ForeignPtr(_) => {
                EnumPayloadKind::INERT
            }
            _ => return Err(LlvmError::internal("an unsupported enum payload")),
        };
        // SAFETY: `value` has the declared payload type and each conversion targets
        // the enum runtime's i64 payload word.
        let word = unsafe {
            match ty {
                Type::Int(_) => value,
                Type::Float(_) => LLVMBuildBitCast(
                    self.builder,
                    value,
                    self.types.i64,
                    c"payload.bits".as_ptr(),
                ),
                Type::Bool => LLVMBuildZExt(
                    self.builder,
                    value,
                    self.types.i64,
                    c"payload.bits".as_ptr(),
                ),
                Type::RawPtr | Type::ForeignPtr(_) => value,
                Type::String | Type::Enum(_) | Type::Any | Type::Cell(_) => LLVMBuildPtrToInt(
                    self.builder,
                    value,
                    self.types.i64,
                    c"payload.bits".as_ptr(),
                ),
                _ => return Err(LlvmError::internal("an unsupported enum payload")),
            }
        };
        Ok((self.const_int(kind.as_i64()), word))
    }

    fn emit_invalid_enum_state(&self, block: LLVMBasicBlockRef) {
        // SAFETY: `block` belongs to the helper being emitted.
        unsafe {
            LLVMPositionBuilderAtEnd(self.builder, block);
            self.call_runtime(
                self.runtime.trap_native_state,
                &mut [self.const_i32(NativeStateStatus::MALFORMED_VALUE.0)],
                c"",
            );
            LLVMBuildUnreachable(self.builder);
        }
    }

    fn append_enum_block(
        &self,
        function: LLVMValueRef,
        name: &std::ffi::CStr,
    ) -> LLVMBasicBlockRef {
        // SAFETY: `function` belongs to this live context.
        unsafe { LLVMAppendBasicBlockInContext(self.context, function, name.as_ptr()) }
    }

    fn enum_phi(
        &self,
        incoming: &mut [(LLVMValueRef, LLVMBasicBlockRef)],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: every incoming value is a pointer and every block branches to
        // the current merge block.
        unsafe {
            let phi = LLVMBuildPhi(self.builder, self.types.ptr, name.as_ptr());
            let mut values: Vec<LLVMValueRef> = incoming.iter().map(|(value, _)| *value).collect();
            let mut blocks: Vec<LLVMBasicBlockRef> =
                incoming.iter().map(|(_, block)| *block).collect();
            LLVMAddIncoming(
                phi,
                values.as_mut_ptr(),
                blocks.as_mut_ptr(),
                values.len() as u32,
            );
            phi
        }
    }
}

#[cfg(test)]
mod tests {
    use kira_runtime_abi::Execution;
    use kira_semantics_model::hir::{HirExpr, HirFunction, HirLocal, HirProgram, HirStmt, LocalId};
    use kira_semantics_model::{EnumDef, OwnershipMode, StructDef, Type, VariantDef};
    use kira_source::Span;

    use crate::codegen::Module;

    #[test]
    fn a_native_state_enum_supports_foreign_pointer_and_cell_payloads() {
        let mut program = HirProgram::default();
        let target = program
            .types
            .structs_mut()
            .declare(StructDef {
                name: "Target".to_owned(),
                fields: Vec::new(),
                c_layout: false,
                drop_glue: None,
            })
            .expect("the pointer target declaration succeeds");
        let pointer = program.types.foreign_ptr_to(target);
        let cell = program.types.cell_of(Type::INT);
        let enum_id = program
            .types
            .enums_mut()
            .declare(EnumDef {
                name: "Payload".to_owned(),
                variants: vec![
                    VariantDef {
                        name: "Pointer".to_owned(),
                        payload: Some(pointer),
                    },
                    VariantDef {
                        name: "Cell".to_owned(),
                        payload: Some(cell),
                    },
                ],
            })
            .expect("the enum declaration succeeds");
        let ty = Type::Enum(enum_id);
        let value = program.exprs.alloc(HirExpr::Local {
            local: LocalId(0),
            ty,
        });
        let ret = program.stmts.alloc(HirStmt::Return { value: Some(value) });
        program.functions.push(HirFunction {
            name: "echoPayload".to_owned(),
            param_count: 1,
            return_type: ty,
            locals: vec![HirLocal {
                name: "payload".to_owned(),
                ty,
                mutable: false,
                ownership: OwnershipMode::Owned,
                native_state: None,
            }],
            body: vec![ret],
            is_main: false,
            is_async: false,
            execution: Execution::Native,
            mutates_self: false,
            name_span: Span::new(0, 11),
        });

        let ir = kira_ir::lower(&program);
        Module::build_hybrid(&ir, "enum_foreign_cell_probe", &[])
            .expect("frontend-valid enum payloads have native-state leaves");
    }
}
