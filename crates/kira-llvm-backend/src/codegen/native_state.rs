//! Native lowering support for opaque callback-state value trees.

use kira_runtime_abi::{NativeStateStatus, NativeStateTypeId, NativeStateValueTag};
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::Codegen;
use super::ffi::c_string;
use super::lower::FunctionLowering;
use crate::LlvmError;

/// Which per-element callback an array state conversion needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) enum StateLeaf {
    /// Consume one native array element into a generic value node.
    Encode,
    /// Consume one generic value node into a fresh native array element.
    Decode,
}

impl FunctionLowering<'_, '_> {
    /// Boxes an owned value and returns its stable opaque token word.
    pub(super) fn lower_native_state_new(
        &mut self,
        value: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.type_of(value);
        let value = self.lower_expr(value)?;
        let node = self.codegen.encode_native_state_value(value, ty)?;
        let out = self.alloca_i64(c"native.state.token");
        let status = self.call(
            self.codegen.runtime.native_state_new,
            &mut [self.codegen.const_int(type_id.as_word() as i64), node, out],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        // SAFETY: `out` is this function's live i64 alloca initialized by the
        // successful runtime call.
        Ok(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                out,
                c"native.state".as_ptr(),
            )
        })
    }

    /// Validates recovery and returns the opaque token word for proxy storage.
    pub(super) fn lower_native_recover_token(
        &mut self,
        raw: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(raw)?;
        let node = self.recover_native_node(token, type_id)?;
        self.call(self.codegen.runtime.native_value_free, &mut [node], c"");
        Ok(token)
    }

    /// Recovers and materializes an owned Kira value.
    pub(super) fn lower_native_recover_value(
        &mut self,
        raw: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(raw)?;
        self.materialize_native_state(token, type_id, ty)
    }

    /// Frees a callback-state token and yields a harmless expression value.
    pub(super) fn lower_native_state_free(
        &mut self,
        token: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(token)?;
        let status = self.call(
            self.codegen.runtime.native_state_free,
            &mut [token],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        Ok(self.codegen.const_bool(false))
    }

    /// Materializes one recovered-view local as an owned Kira value.
    pub(super) fn load_native_state_local(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let pointer = self.local_pointer(slot)?;
        // SAFETY: a recovered-view local is allocated as an i64 token slot.
        let token = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                pointer,
                c"native.token".as_ptr(),
            )
        };
        self.materialize_native_state(token, type_id, ty)
    }

    /// Replaces the state behind a recovered-view local, consuming `value`.
    pub(super) fn replace_native_state_local(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        let pointer = self.local_pointer(slot)?;
        // SAFETY: a recovered-view local is allocated as an i64 token slot.
        let token = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                pointer,
                c"native.token".as_ptr(),
            )
        };
        let node = self.codegen.encode_native_state_value(value, ty)?;
        let status = self.call(
            self.codegen.runtime.native_state_replace,
            &mut [
                token,
                self.codegen.const_int(type_id.as_word() as i64),
                node,
            ],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        Ok(())
    }

    /// Recovers a proxy local into temporary addressable storage.
    pub(super) fn recover_native_state_alloca(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<(LLVMValueRef, LLVMValueRef), LlvmError> {
        let value = self.load_native_state_local(slot, type_id, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        let pointer = self.alloca(llvm_type, c"native.view.value");
        // SAFETY: `pointer` is a fresh alloca of `llvm_type` and `value` has it.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok((pointer, value))
    }

    fn materialize_native_state(
        &mut self,
        token: LLVMValueRef,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let node = self.recover_native_node(token, type_id)?;
        self.codegen.decode_native_state_value(node, ty)
    }

    fn recover_native_node(
        &mut self,
        token: LLVMValueRef,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let out = self.alloca(self.codegen.types.ptr, c"native.state.node");
        let status = self.call(
            self.codegen.runtime.native_state_recover,
            &mut [token, self.codegen.const_int(type_id.as_word() as i64), out],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        // SAFETY: `out` is a live pointer alloca initialized on success.
        Ok(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.ptr,
                out,
                c"native.node".as_ptr(),
            )
        })
    }

    fn alloca_i64(&self, name: &std::ffi::CStr) -> LLVMValueRef {
        self.alloca(self.codegen.types.i64, name)
    }

    fn alloca(&self, ty: LLVMTypeRef, name: &std::ffi::CStr) -> LLVMValueRef {
        // SAFETY: the builder is on a live function block and `ty` belongs to the
        // module's context.
        unsafe { LLVMBuildAlloca(self.codegen.builder, ty, name.as_ptr()) }
    }

    pub(super) fn check_native_state_status(&self, status: LLVMValueRef) {
        let builder = self.codegen.builder;
        let context = self.codegen.context;
        // SAFETY: the builder is on a live function block; all blocks are added to
        // that function and every value belongs to this context.
        unsafe {
            let current = LLVMGetInsertBlock(builder);
            let function = LLVMGetBasicBlockParent(current);
            let ok = LLVMAppendBasicBlockInContext(context, function, c"native.state.ok".as_ptr());
            let trap =
                LLVMAppendBasicBlockInContext(context, function, c"native.state.trap".as_ptr());
            let zero = LLVMConstInt(self.codegen.types.i32, 0, 0);
            let clean = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                status,
                zero,
                c"native.state.clean".as_ptr(),
            );
            LLVMBuildCondBr(builder, clean, ok, trap);
            LLVMPositionBuilderAtEnd(builder, trap);
            self.codegen
                .call_runtime(self.codegen.runtime.trap_native_state, &mut [status], c"");
            LLVMBuildUnreachable(builder);
            LLVMPositionBuilderAtEnd(builder, ok);
        }
    }
}

impl Codegen<'_> {
    /// Consumes one native Kira value into a generic state-value node.
    pub(super) fn encode_native_state_value(
        &mut self,
        value: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        Ok(match ty {
            Type::Int(_) => self.call(self.runtime.native_value_int, &mut [value], c"native.value"),
            Type::Float(_) => self.call(
                self.runtime.native_value_float,
                &mut [value],
                c"native.value",
            ),
            Type::Bool => {
                // SAFETY: `value` is i1 and the runtime's C ABI takes i8.
                let byte = unsafe {
                    LLVMBuildZExt(self.builder, value, self.types.i8, c"native.bool".as_ptr())
                };
                self.call(self.runtime.native_value_bool, &mut [byte], c"native.value")
            }
            Type::String => self.call(
                self.runtime.native_value_string,
                &mut [value],
                c"native.value",
            ),
            Type::RawPtr => self.call(
                self.runtime.native_value_raw_ptr,
                &mut [value],
                c"native.value",
            ),
            Type::Struct(id) => {
                let fields = self.field_types(id)?;
                let node = self.aggregate_node(NativeStateValueTag::STRUCT, 0, fields.len());
                for (index, field_ty) in fields.into_iter().enumerate() {
                    let field = self.extract_field(value, index as u32);
                    let child = self.encode_native_state_value(field, field_ty)?;
                    self.set_native_child(node, index, child);
                }
                node
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let encode = self.native_state_element_leaf(element, StateLeaf::Encode)?;
                self.call(
                    self.runtime.native_value_array_from,
                    &mut [value, esize, encode],
                    c"native.array.value",
                )
            }
            Type::Enum(id) => {
                let encoder = self.native_state_enum_leaf(id, StateLeaf::Encode)?;
                self.call(encoder, &mut [value], c"native.enum.value")
            }
            Type::Void | Type::Error | Type::CString | Type::NativeState(_) => {
                return Err(LlvmError::Unsupported(
                    "a non-Kira-owned native callback-state value",
                ));
            }
        })
    }

    /// Consumes one generic node into an owned native Kira value.
    pub(super) fn decode_native_state_value(
        &mut self,
        node: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        Ok(match ty {
            Type::Int(_) => self.read_and_free_node(node, self.runtime.native_value_read_int),
            Type::Float(_) => self.read_and_free_node(node, self.runtime.native_value_read_float),
            Type::Bool => {
                let byte = self.read_and_free_node(node, self.runtime.native_value_read_bool);
                // SAFETY: the runtime returns a C boolean byte.
                unsafe {
                    LLVMBuildTrunc(self.builder, byte, self.types.i1, c"native.bool".as_ptr())
                }
            }
            Type::String => self.read_and_free_node(node, self.runtime.native_value_read_string),
            Type::RawPtr => self.read_and_free_node(node, self.runtime.native_value_read_raw_ptr),
            Type::Struct(id) => {
                let fields = self.field_types(id)?;
                let llvm_type = self.llvm_type(ty)?;
                // SAFETY: this type belongs to the live context.
                let mut value = unsafe { LLVMGetUndef(llvm_type) };
                for (index, field_ty) in fields.into_iter().enumerate() {
                    let child = self.call(
                        self.runtime.native_value_child,
                        &mut [node, self.const_int(index as i64)],
                        c"native.child",
                    );
                    let field = self.decode_native_state_value(child, field_ty)?;
                    value = self.insert_field(value, field, index as u32);
                }
                self.call(self.runtime.native_value_free, &mut [node], c"");
                value
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let decode = self.native_state_element_leaf(element, StateLeaf::Decode)?;
                self.call(
                    self.runtime.native_value_array_to,
                    &mut [node, esize, decode],
                    c"native.array",
                )
            }
            Type::Enum(id) => {
                let decoder = self.native_state_enum_leaf(id, StateLeaf::Decode)?;
                self.call(decoder, &mut [node], c"native.enum")
            }
            Type::Void | Type::Error | Type::CString | Type::NativeState(_) => {
                return Err(LlvmError::Unsupported(
                    "a non-Kira-owned native callback-state value",
                ));
            }
        })
    }

    fn aggregate_node(
        &self,
        tag: NativeStateValueTag,
        enum_tag: u32,
        count: usize,
    ) -> LLVMValueRef {
        self.aggregate_node_dynamic(tag, self.const_i32(enum_tag), count)
    }

    pub(in crate::codegen) fn aggregate_node_dynamic(
        &self,
        tag: NativeStateValueTag,
        enum_tag: LLVMValueRef,
        count: usize,
    ) -> LLVMValueRef {
        self.call(
            self.runtime.native_value_aggregate,
            &mut [
                self.const_i32(tag.0),
                enum_tag,
                self.const_int(count as i64),
            ],
            c"native.aggregate",
        )
    }

    pub(in crate::codegen) fn set_native_child(
        &self,
        node: LLVMValueRef,
        index: usize,
        child: LLVMValueRef,
    ) {
        let status = self.call(
            self.runtime.native_value_set_child,
            &mut [node, self.const_int(index as i64), child],
            c"native.child.status",
        );
        self.check_native_status_in_codegen(status);
    }

    fn read_and_free_node(&self, node: LLVMValueRef, reader: super::Callable) -> LLVMValueRef {
        let value = self.call(reader, &mut [node], c"native.value");
        self.call(self.runtime.native_value_free, &mut [node], c"");
        value
    }

    fn check_native_status_in_codegen(&self, status: LLVMValueRef) {
        let builder = self.builder;
        // SAFETY: same control-flow construction as FunctionLowering's status
        // check, but needed by recursive helpers that live on Codegen.
        unsafe {
            let current = LLVMGetInsertBlock(builder);
            let function = LLVMGetBasicBlockParent(current);
            let ok =
                LLVMAppendBasicBlockInContext(self.context, function, c"native.node.ok".as_ptr());
            let trap =
                LLVMAppendBasicBlockInContext(self.context, function, c"native.node.trap".as_ptr());
            let clean = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                status,
                LLVMConstInt(self.types.i32, u64::from(NativeStateStatus::OK.0), 0),
                c"native.node.clean".as_ptr(),
            );
            LLVMBuildCondBr(builder, clean, ok, trap);
            LLVMPositionBuilderAtEnd(builder, trap);
            self.call_runtime(self.runtime.trap_native_state, &mut [status], c"");
            LLVMBuildUnreachable(builder);
            LLVMPositionBuilderAtEnd(builder, ok);
        }
    }

    pub(super) fn native_state_element_leaf(
        &mut self,
        ty: Type,
        leaf: StateLeaf,
    ) -> Result<LLVMValueRef, LlvmError> {
        if let Some(cached) = self.native_state_leaves.get(&(ty, leaf)) {
            return Ok(*cached);
        }
        let ordinal = self.native_state_leaves.len();
        let name = c_string(&match leaf {
            StateLeaf::Encode => format!("kira.native.state.encode.{ordinal}"),
            StateLeaf::Decode => format!("kira.native.state.decode.{ordinal}"),
        });
        let mut params = match leaf {
            StateLeaf::Encode => vec![self.types.ptr],
            StateLeaf::Decode => vec![self.types.ptr, self.types.ptr],
        };
        let result = match leaf {
            StateLeaf::Encode => self.types.ptr,
            StateLeaf::Decode => self.types.void,
        };
        // SAFETY: all types belong to this context and the parameter slice lives
        // through the declaration.
        let function = unsafe {
            let signature = LLVMFunctionType(result, params.as_mut_ptr(), params.len() as u32, 0);
            let function = LLVMAddFunction(self.module, name.as_ptr(), signature);
            LLVMSetLinkage(function, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            function
        };
        self.native_state_leaves.insert((ty, leaf), function);
        // SAFETY: the function is live and gets one fresh entry block.
        let entry =
            unsafe { LLVMAppendBasicBlockInContext(self.context, function, c"entry".as_ptr()) };
        // SAFETY: save and restore the caller's live builder position.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: `entry` belongs to this module's function.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };
        let emitted: Result<(), LlvmError> = match leaf {
            StateLeaf::Encode => {
                // SAFETY: parameter zero points at one live element of `ty`.
                let source = unsafe { LLVMGetParam(function, 0) };
                let llvm_type = self.llvm_type(ty)?;
                // SAFETY: the callback contract supplies a slot of this type.
                let value =
                    unsafe { LLVMBuildLoad2(self.builder, llvm_type, source, c"element".as_ptr()) };
                let node = self.encode_native_state_value(value, ty)?;
                // SAFETY: the node is the function's pointer result.
                unsafe { LLVMBuildRet(self.builder, node) };
                Ok(())
            }
            StateLeaf::Decode => {
                // SAFETY: the signature has two pointer parameters.
                let node = unsafe { LLVMGetParam(function, 0) };
                // SAFETY: same signature guarantee.
                let target = unsafe { LLVMGetParam(function, 1) };
                let value = self.decode_native_state_value(node, ty)?;
                // SAFETY: `target` is a fresh slot of `ty` and `value` has it.
                unsafe {
                    LLVMBuildStore(self.builder, value, target);
                    LLVMBuildRetVoid(self.builder);
                }
                Ok(())
            }
        };
        // SAFETY: `resume` is the previously live block, when one existed.
        unsafe {
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        emitted?;
        Ok(function)
    }

    pub(in crate::codegen) fn const_i32(&self, value: u32) -> LLVMValueRef {
        // SAFETY: i32 belongs to this live context.
        unsafe { LLVMConstInt(self.types.i32, u64::from(value), 0) }
    }
}
