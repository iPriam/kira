//! The backend-neutral value tree a hybrid half's callback state is kept in.
//!
//! One Kira value becomes a tree of `kira_rt_native_value_*` nodes on the way
//! out and comes back the same way, so the VM and the native half share one
//! shape for state neither engine's storage means anything to the other in.
//! The same pair of walks carries a struct, an array or a payload-carrying enum
//! across the `@Native`/`@Runtime` seam ([`super::bridge`]), which is why they
//! live on [`Codegen`] rather than on one function's lowering: a value's shape
//! is a program-wide fact, and an array's per-element leaf is emitted with no
//! function body in scope.
//!
//! See [`super::native_state`] for the other shape callback state can take —
//! the box a whole-program native module uses instead.

use kira_runtime_abi::{NativeStateStatus, NativeStateValueTag};
use kira_semantics_model::{ErasedTypeId, Type};
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::ffi::c_string;
use super::native_state::StateLeaf;
use super::{Callable, Codegen};
use crate::LlvmError;

/// Names the shape that has no callback-state node form.
///
/// Every one of these is a shape the VM's `Heap::into_native_state` refuses
/// too, and for the same reason each names: a node tree is a copy of a value,
/// and none of these is a value one engine can copy into the other's world.
/// Naming which one it was is what tells a refusal deep inside a struct apart
/// from a refusal of the struct itself.
fn no_node_form(ty: Type) -> LlvmError {
    LlvmError::internal(match ty {
        // `void` has no LLVM value at all, so there is nothing to encode and
        // nothing a decode could produce.
        Type::Void => "a void value inside native callback state",
        // A task handle names work owned by the scheduler that spawned it, and
        // a copy of the handle in the other engine would name nothing there.
        Type::Task(_) => "a task handle inside native callback state",
        // State inside state: the inner token names a box in a store the outer
        // value knows nothing about.
        Type::NativeState(_) => "native callback state inside native callback state",
        // `kira-ir` rewrites a distinct type to the scalar it is before a
        // backend runs, so one here means that pass was skipped rather than
        // that the shape has no node form — the scalar underneath has one.
        Type::Distinct(_) => "a distinct type that lowering did not erase",
        // Lowering only ever runs on a program that type-checked.
        _ => "a program that failed to type-check",
    })
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
            Type::CBlock => self.call(
                self.runtime.native_value_cblock_from_handle,
                &mut [value],
                c"native.cblock",
            ),
            // Three spellings of one opaque target word, so one node carries
            // them all. A `CString` member of a C-layout struct is the address
            // of bytes some foreign library owns: this side stores it, hands it
            // back, and never dereferences or frees it, which is exactly what
            // `RawPtr` already is here and exactly what the VM keeps such a
            // member in (`Value::RawPtr`, see `kira-vm-runtime`'s `aggregate`
            // module). Encoding it as anything else would make the two engines
            // disagree about one word of a shared struct.
            Type::RawPtr | Type::ForeignPtr(_) | Type::CString => self.call(
                self.runtime.native_value_raw_ptr,
                &mut [value],
                c"native.value",
            ),
            Type::Struct(id) => {
                let def = self
                    .program
                    .types
                    .structs()
                    .get(id)
                    .ok_or(LlvmError::internal(
                        "a native-state struct not in the table",
                    ))?
                    .clone();
                let node = self.aggregate_node(NativeStateValueTag::STRUCT, 0, def.fields.len());
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let field = self.extract_field(value, index as u32);
                    let child = if def.owns_c_storage_at(index as u32) {
                        self.encode_native_state_value(field, Type::CBlock)?
                    } else {
                        self.encode_native_state_value(field, field_ty)?
                    };
                    self.set_native_child(node, index, child);
                }
                node
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let clone = self.element_clone(element)?;
                let encode = self.native_state_element_leaf(element, StateLeaf::Encode)?;
                self.call(
                    self.runtime.native_value_array_from,
                    &mut [value, esize, clone, encode.value],
                    c"native.array.value",
                )
            }
            Type::Enum(id) => {
                let encoder = self.native_state_enum_leaf(id, StateLeaf::Encode)?;
                self.call(encoder, &mut [value], c"native.enum.value")
            }
            // The node takes a share, so release the consumed source handle
            // after the bridge has retained its own copy. This also applies
            // when the cell is a field of an aggregate copied from an enum
            // payload.
            Type::Cell(_) => {
                let node = self.call(
                    self.runtime.native_value_cell,
                    &mut [value],
                    c"native.cell.value",
                );
                self.call(self.runtime.cell_free, &mut [value], c"");
                node
            }
            Type::Any => self.encode_any_node_through_leaf(value)?,
            Type::Void
            | Type::Error
            | Type::Distinct(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::NativeState(_) => {
                return Err(no_node_form(ty));
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
            Type::CBlock => self.call(
                self.runtime.native_value_cblock_to_handle,
                &mut [node],
                c"native.cblock",
            ),
            // The mirror of the encode side: one opaque word comes back out of
            // the node it went into, for `CString` exactly as for `RawPtr`.
            Type::RawPtr | Type::ForeignPtr(_) | Type::CString => {
                self.read_and_free_node(node, self.runtime.native_value_read_raw_ptr)
            }
            Type::Struct(id) => {
                let def = self
                    .program
                    .types
                    .structs()
                    .get(id)
                    .ok_or(LlvmError::internal(
                        "a native-state struct not in the table",
                    ))?
                    .clone();
                let llvm_type = self.llvm_type(ty)?;
                // SAFETY: this type belongs to the live context.
                let mut value = unsafe { LLVMGetUndef(llvm_type) };
                for (index, field_ty) in def.fields.iter().map(|field| field.ty).enumerate() {
                    let child = self.call(
                        self.runtime.native_value_child,
                        &mut [node, self.const_int(index as i64)],
                        c"native.child",
                    );
                    let field = if def.owns_c_storage_at(index as u32) {
                        self.decode_native_state_value(child, Type::CBlock)?
                    } else {
                        self.decode_native_state_value(child, field_ty)?
                    };
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
                    &mut [node, esize, decode.value],
                    c"native.array",
                )
            }
            Type::Enum(id) => {
                let decoder = self.native_state_enum_leaf(id, StateLeaf::Decode)?;
                self.call(decoder, &mut [node], c"native.enum")
            }
            // The box back out, with a share of its own, and the node freed
            // like every other decode frees the node it consumed.
            Type::Cell(_) => {
                let cell = self.call(
                    self.runtime.native_value_read_cell,
                    &mut [node],
                    c"native.cell",
                );
                self.call(self.runtime.native_value_free, &mut [node], c"");
                cell
            }
            Type::Any => self.decode_any_node_through_leaf(node)?,
            Type::Void
            | Type::Error
            | Type::Distinct(_)
            | Type::Task(_)
            | Type::MainThreadTask(_)
            | Type::NativeState(_) => {
                return Err(no_node_form(ty));
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

    /// Calls the recursive erased-value encoder through its generated leaf.
    ///
    /// The leaf is what makes a struct containing an `Any` recursive without
    /// trying to emit an infinitely nested switch inline. The value slot is
    /// temporary because the leaf's ABI is the same pointer-to-element shape
    /// used by array callbacks.
    fn encode_any_node_through_leaf(
        &mut self,
        value: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(Type::Any)?;
        let (source, saved) = self.dynamic_alloca(llvm_type, c"native.any.source");
        self.lifetime_start(source);
        // SAFETY: `source` has the LLVM representation of `Any`, and `value`
        // was produced with that same type.
        unsafe { LLVMBuildStore(self.builder, value, source) };
        let encoder = self.native_state_element_leaf(Type::Any, StateLeaf::Encode)?;
        let node = self.call(encoder, &mut [source], c"native.any");
        self.release_dynamic_alloca(source, saved);
        Ok(node)
    }

    /// Calls the recursive erased-value decoder through its generated leaf.
    fn decode_any_node_through_leaf(
        &mut self,
        node: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(Type::Any)?;
        let (target, saved) = self.dynamic_alloca(llvm_type, c"native.any.target");
        self.lifetime_start(target);
        let decoder = self.native_state_element_leaf(Type::Any, StateLeaf::Decode)?;
        self.call(decoder, &mut [node, target], c"native.any");
        // SAFETY: the generated decoder stores one owned `Any` in `target`.
        let value = unsafe {
            LLVMBuildLoad2(
                self.builder,
                llvm_type,
                target,
                c"native.any.value".as_ptr(),
            )
        };
        self.release_dynamic_alloca(target, saved);
        Ok(value)
    }

    /// Encodes one native `Any` box into a typed node tree.
    ///
    /// The box's `ErasedTypeId` is the only runtime type information available
    /// at this point. The cases are the program's concrete erasable types, so
    /// every selected arm re-enters the ordinary typed encoder and retains all
    /// of its aggregate and ownership rules.
    fn encode_any_node(&mut self, value: LLVMValueRef) -> Result<LLVMValueRef, LlvmError> {
        let type_id = self.call(self.runtime.enum_tag, &mut [value], c"native.any.type");
        // SAFETY: the builder is positioned in this helper's entry block, so it
        // has an insert block and that block has a parent function.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let merge = self.append_any_block(function, "native.any.encode.merge");
        let invalid = self.append_any_block(function, "native.any.encode.invalid");
        // SAFETY: `type_id` is the i64 tag in the enum-shaped `Any` box, and
        // every case constant below is an i64 erased type identity.
        let switch = unsafe {
            LLVMBuildSwitch(
                self.builder,
                type_id,
                invalid,
                self.erased_types().len() as u32,
            )
        };
        let mut incoming = Vec::new();
        for ty in self.erased_types() {
            let Some(id) = ErasedTypeId::of(ty) else {
                continue;
            };
            let block = self.append_any_block(function, "native.any.encode.case");
            // SAFETY: `switch` and `block` belong to this helper function.
            unsafe { LLVMAddCase(switch, self.const_int(id.as_i64()), block) };
            // SAFETY: `block` is fresh and belongs to this function.
            unsafe { LLVMPositionBuilderAtEnd(self.builder, block) };
            let payload = self.read_box_payload(value, ty)?;
            let child = self.encode_native_state_value(payload, ty)?;
            let node = self.call(
                self.runtime.native_value_any,
                &mut [self.const_int(id.as_i64()), child],
                c"native.any.node",
            );
            self.call(self.runtime.enum_free, &mut [value], c"native.any.free");
            // SAFETY: nested conversions may finish in a successor block, and
            // the branch below is emitted on that live predecessor.
            unsafe { LLVMBuildBr(self.builder, merge) };
            // SAFETY: the branch above left the builder in the block that
            // reaches `merge`, which is the phi's predecessor.
            let predecessor = unsafe { LLVMGetInsertBlock(self.builder) };
            incoming.push((node, predecessor));
        }
        self.emit_invalid_any_state(invalid);
        // SAFETY: `merge` receives one pointer from every valid erased type.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, merge) };
        // The builder is left on `merge`, unterminated: the leaf that called
        // this owns its own return.
        Ok(self.any_pointer_phi(&mut incoming, c"native.any.node"))
    }

    /// Decodes one dynamic node into a freshly boxed native `Any` value.
    fn decode_any_node(&mut self, node: LLVMValueRef) -> Result<LLVMValueRef, LlvmError> {
        let type_id = self.call(
            self.runtime.native_value_read_any_type,
            &mut [node],
            c"native.any.type",
        );
        // SAFETY: the builder is positioned in this helper's entry block, so it
        // has an insert block and that block has a parent function.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        let merge = self.append_any_block(function, "native.any.decode.merge");
        let invalid = self.append_any_block(function, "native.any.decode.invalid");
        // SAFETY: the node accessor returns the i64 identity stored by the
        // encoder, and all cases are i64 constants from the same program.
        let switch = unsafe {
            LLVMBuildSwitch(
                self.builder,
                type_id,
                invalid,
                self.erased_types().len() as u32,
            )
        };
        let mut incoming = Vec::new();
        for ty in self.erased_types() {
            let Some(id) = ErasedTypeId::of(ty) else {
                continue;
            };
            let block = self.append_any_block(function, "native.any.decode.case");
            // SAFETY: `switch` and `block` belong to this helper function.
            unsafe { LLVMAddCase(switch, self.const_int(id.as_i64()), block) };
            // SAFETY: `block` is fresh and belongs to this function.
            unsafe { LLVMPositionBuilderAtEnd(self.builder, block) };
            let child = self.call(
                self.runtime.native_value_child,
                &mut [node, self.const_int(0)],
                c"native.any.payload",
            );
            let payload = self.decode_native_state_value(child, ty)?;
            let boxed =
                self.box_new(self.const_int(id.as_i64()), ty, payload, c"native.any.box")?;
            self.call(
                self.runtime.native_value_free,
                &mut [node],
                c"native.any.free",
            );
            // SAFETY: nested conversions may finish in a successor block, and
            // the branch below is emitted on that live predecessor.
            unsafe { LLVMBuildBr(self.builder, merge) };
            // SAFETY: the branch above left the builder in the block that
            // reaches `merge`, which is the phi's predecessor.
            let predecessor = unsafe { LLVMGetInsertBlock(self.builder) };
            incoming.push((boxed, predecessor));
        }
        self.emit_invalid_any_state(invalid);
        // SAFETY: `merge` receives one pointer from every valid erased type.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, merge) };
        // The builder is left on `merge`, unterminated: the leaf that called
        // this owns its own return.
        Ok(self.any_pointer_phi(&mut incoming, c"native.any"))
    }

    /// The concrete types whose erased identities can occur in this program.
    fn erased_types(&self) -> Vec<Type> {
        let mut types = vec![
            Type::INT,
            Type::FLOAT,
            Type::Bool,
            Type::String,
            Type::RawPtr,
        ];
        types.extend(self.program.types.structs().ids().map(Type::Struct));
        types.extend(
            self.program
                .types
                .arrays()
                .rows()
                .map(|(id, _)| Type::Array(id)),
        );
        types.extend(self.program.types.enums().ids().map(Type::Enum));
        types
    }

    fn append_any_block(&self, function: LLVMValueRef, name: &str) -> LLVMBasicBlockRef {
        let name = c_string(name);
        // SAFETY: `function` belongs to this live LLVM context.
        unsafe { LLVMAppendBasicBlockInContext(self.context, function, name.as_ptr()) }
    }

    fn emit_invalid_any_state(&self, block: LLVMBasicBlockRef) {
        // SAFETY: `block` belongs to the helper currently being emitted.
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

    fn any_pointer_phi(
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

    fn read_and_free_node(&self, node: LLVMValueRef, reader: Callable) -> LLVMValueRef {
        let value = self.call(reader, &mut [node], c"native.value");
        self.call(self.runtime.native_value_free, &mut [node], c"");
        value
    }

    fn emit_any_state_leaf(
        &mut self,
        function: LLVMValueRef,
        leaf: StateLeaf,
    ) -> Result<(), LlvmError> {
        match leaf {
            StateLeaf::Encode => {
                // SAFETY: parameter zero points at one live `Any` value.
                let source = unsafe { LLVMGetParam(function, 0) };
                let llvm_type = self.llvm_type(Type::Any)?;
                // SAFETY: the callback contract supplies a slot of this type.
                let value = unsafe {
                    LLVMBuildLoad2(self.builder, llvm_type, source, c"any.element".as_ptr())
                };
                let node = self.encode_any_node(value)?;
                // SAFETY: the node is the helper's pointer result.
                unsafe { LLVMBuildRet(self.builder, node) };
                Ok(())
            }
            StateLeaf::Decode => {
                // SAFETY: the signature has two pointer parameters.
                let node = unsafe { LLVMGetParam(function, 0) };
                // SAFETY: the second parameter is a writable `Any` slot.
                let target = unsafe { LLVMGetParam(function, 1) };
                let value = self.decode_any_node(node)?;
                // SAFETY: `target` has the LLVM representation of `Any` and
                // `value` is the freshly boxed pointer returned above.
                unsafe {
                    LLVMBuildStore(self.builder, value, target);
                    LLVMBuildRetVoid(self.builder);
                }
                Ok(())
            }
        }
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
    ) -> Result<Callable, LlvmError> {
        let mut params = match leaf {
            StateLeaf::Encode => vec![self.types.ptr],
            StateLeaf::Decode => vec![self.types.ptr, self.types.ptr],
        };
        let result = match leaf {
            StateLeaf::Encode => self.types.ptr,
            StateLeaf::Decode => self.types.void,
        };
        // SAFETY: all types belong to this context and the parameter slice lives
        // through the declaration type construction.
        let signature =
            unsafe { LLVMFunctionType(result, params.as_mut_ptr(), params.len() as u32, 0) };
        if let Some(cached) = self.native_state_leaves.get(&(ty, leaf)) {
            return Ok(Callable {
                ty: signature,
                value: *cached,
            });
        }
        let ordinal = self.native_state_leaves.len();
        let name = c_string(&match leaf {
            StateLeaf::Encode => format!("kira.native.state.encode.{ordinal}"),
            StateLeaf::Decode => format!("kira.native.state.decode.{ordinal}"),
        });
        // SAFETY: all types belong to this context and the parameter slice lives
        // through the declaration.
        let function = unsafe {
            let function = LLVMAddFunction(self.module, name.as_ptr(), signature);
            LLVMSetLinkage(function, llvm_sys::LLVMLinkage::LLVMInternalLinkage);
            function
        };
        let callable = Callable {
            ty: signature,
            value: function,
        };
        self.native_state_leaves.insert((ty, leaf), callable.value);
        // SAFETY: the function is live and gets one fresh entry block.
        let entry =
            unsafe { LLVMAppendBasicBlockInContext(self.context, function, c"entry".as_ptr()) };
        // SAFETY: save and restore the caller's live builder position.
        let resume = unsafe { LLVMGetInsertBlock(self.builder) };
        // SAFETY: `entry` belongs to this module's function.
        unsafe { LLVMPositionBuilderAtEnd(self.builder, entry) };
        let emitted: Result<(), LlvmError> = if ty == Type::Any {
            self.emit_any_state_leaf(function, leaf)
        } else {
            match leaf {
                StateLeaf::Encode => {
                    // SAFETY: parameter zero points at one live element of `ty`.
                    let source = unsafe { LLVMGetParam(function, 0) };
                    let llvm_type = self.llvm_type(ty)?;
                    // SAFETY: the callback contract supplies a slot of this type.
                    let value = unsafe {
                        LLVMBuildLoad2(self.builder, llvm_type, source, c"element".as_ptr())
                    };
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
            }
        };
        // SAFETY: `resume` is the previously live block, when one existed.
        unsafe {
            if !resume.is_null() {
                LLVMPositionBuilderAtEnd(self.builder, resume);
            }
        }
        emitted?;
        Ok(callable)
    }

    pub(in crate::codegen) fn const_i32(&self, value: u32) -> LLVMValueRef {
        // SAFETY: i32 belongs to this live context.
        unsafe { LLVMConstInt(self.types.i32, u64::from(value), 0) }
    }
}
