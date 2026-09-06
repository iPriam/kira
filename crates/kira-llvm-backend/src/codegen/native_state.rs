//! Native lowering for callback state, in the two shapes it can take.
//!
//! # A box, wherever one will do
//!
//! A whole-program native module owns the layout of every value it compiles, so
//! its callback state is a box holding that value — `nativeState` allocates one
//! and stores into it, `nativeRecover` is the box's address, reading a field is
//! a load and writing one is a store. Nothing is encoded, decoded, or copied,
//! which is what lets a compositor recover its state per quad inside a frame
//! budget.
//!
//! # A value tree, when the two halves have to agree
//!
//! The native half of a *hybrid* program cannot do that. State created by a
//! `@Runtime` function lives on the VM's heap, which has no native layout, and
//! a `@Native` function must still be able to recover it. So a hybrid half
//! keeps the backend-neutral value tree: the two engines share one shape and
//! pay to convert. That tree — and the walks that build and read it — lives in
//! [`super::native_state_values`], because it also carries every struct, array
//! and enum crossing the `@Native`/`@Runtime` seam, whether or not any callback
//! state is involved.
//!
//! [`ModuleKind::HybridLibrary`] is the whole of that distinction, and
//! [`FunctionLowering::state_is_boxed`] is where it is read.

use kira_runtime_abi::NativeStateTypeId;
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

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
    /// Whether this module keeps callback state in a box of its own layout.
    ///
    /// Every module but a hybrid half does. See this module's header for why
    /// that one cannot.
    pub(super) fn state_is_boxed(&self) -> bool {
        !matches!(self.codegen.kind, super::ModuleKind::HybridLibrary)
    }

    /// Boxes an owned value and returns its stable opaque token word.
    ///
    /// The box holds the value in this backend's own layout, so nothing is
    /// encoded on the way in and nothing is decoded on the way out — recovering
    /// it later is an address, and reading a field of it is a load. The value
    /// is stored into the box's payload rather than copied into it: the
    /// expression already produced an owned value, and the box takes it.
    pub(super) fn lower_native_state_new(
        &mut self,
        value: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !self.state_is_boxed() {
            return self.lower_native_state_new_tree(value, type_id);
        }
        let ty = self.type_of(value);
        let value = self.lower_expr(value)?;
        let size = self.codegen.abi_size(ty)?;
        let align = self.codegen.abi_align(ty)?;
        // What the value's fields own, and so what releasing the state owes
        // them. Null when the value owns nothing, exactly as an array of `Int`
        // needs no element leaf.
        let free = self.codegen.element_free(ty)?;
        let out = self.alloca_i64(c"native.state.token");
        let status = self.call(
            self.codegen.runtime.native_state_box_new,
            &mut [
                self.codegen.const_int(type_id.as_word() as i64),
                size,
                align,
                free,
                out,
            ],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        // SAFETY: `out` is this function's live i64 alloca initialized by the
        // successful runtime call.
        let token = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                out,
                c"native.state".as_ptr(),
            )
        };
        let payload = self.native_state_payload(token, type_id)?;
        // A fresh box's payload holds no value yet, so this initializes it
        // rather than replacing one — no old value to drop.
        // SAFETY: the payload addresses uninitialized storage sized and aligned
        // for exactly this value's type.
        unsafe { LLVMBuildStore(self.codegen.builder, value, payload) };
        Ok(token)
    }

    /// Validates recovery and returns the opaque token word for proxy storage.
    ///
    /// The recovery is the check: asking for the payload of the wrong type
    /// fails here, where the program said `nativeRecover<T>`, rather than
    /// somewhere downstream reading another type's bytes.
    pub(super) fn lower_native_recover_token(
        &mut self,
        raw: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(raw)?;
        if self.state_is_boxed() {
            self.native_state_payload(token, type_id)?;
        } else {
            let node = self.recover_native_node(token, type_id)?;
            self.call(self.codegen.runtime.native_value_free, &mut [node], c"");
        }
        Ok(token)
    }

    /// The value-tree form of [`Self::lower_native_state_new`].
    fn lower_native_state_new_tree(
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

    /// Recovers one state value node from the value-tree store.
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

    /// Takes the whole state out as a value and gives up the token.
    ///
    /// The token is lowered once and used twice — materialising through it and
    /// then releasing it — so a `raw` with an effect in it happens once, which
    /// lowering the expression twice would not guarantee.
    pub(super) fn lower_native_state_take(
        &mut self,
        raw: kira_ir::IrExprId,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(raw)?;
        let value = self.materialize_native_state(token, type_id, ty)?;
        let status = self.call(
            self.codegen.runtime.native_state_release,
            &mut [token],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        Ok(value)
    }

    /// Exports a handle's userdata token, which owns one reference.
    ///
    /// A handle a local keeps owning is read in place — an ordinary read would
    /// take it — and the token takes a reference of its own. A temporary
    /// handle hands the token the reference it held.
    pub(super) fn lower_native_user_data(
        &mut self,
        state: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let kira_ir::IrExpr::Local(slot) = *self.codegen.program.expr(state) else {
            return self.lower_expr(state);
        };
        if self
            .function
            .native_state_locals
            .get(slot as usize)
            .copied()
            .flatten()
            .is_some()
        {
            return self.lower_expr(state);
        }
        let pointer = self.local_pointer(slot)?;
        // SAFETY: a handle local is an i64 token slot in the entry block.
        let token = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                pointer,
                c"native.state.handle".as_ptr(),
            )
        };
        let status = self.call(
            self.codegen.runtime.native_state_retain,
            &mut [token],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        Ok(token)
    }

    /// Adds one owner to a callback state and yields a harmless expression value.
    pub(super) fn lower_native_state_retain(
        &mut self,
        token: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(token)?;
        let status = self.call(
            self.codegen.runtime.native_state_retain,
            &mut [token],
            c"native.state.status",
        );
        self.check_native_state_status(status);
        Ok(self.codegen.const_bool(false))
    }

    /// Removes one owner from a callback state and yields a harmless
    /// expression value.
    pub(super) fn lower_native_state_release(
        &mut self,
        token: kira_ir::IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.lower_expr(token)?;
        let status = self.call(
            self.codegen.runtime.native_state_release,
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

    /// Replaces the whole state behind a recovered-view local, consuming
    /// `value`.
    ///
    /// A store through the box's payload: the value that was there is dropped
    /// and the new one takes its place, in the storage the state already had.
    /// Nothing is re-encoded, and the token does not change.
    pub(super) fn replace_native_state_local(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        if self.state_is_boxed() {
            let payload = self.native_state_payload_of_local(slot, type_id)?;
            return self.store_through_payload(payload, ty, value);
        }
        let token = self.native_state_token_of_local(slot)?;
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

    /// The address of the value one recovered-view local names.
    ///
    /// This is what a field read or a field write walks from, and it is the
    /// point of the whole design: the state stays where it is, and a path into
    /// it is arithmetic on this address. Nothing is copied out to be read and
    /// nothing is written back afterwards.
    /// Returns that address, and whether the caller owes a write-back.
    ///
    /// A boxed state needs none: the address *is* the state, so a write through
    /// it already landed. A hybrid half's state is a value tree, so the caller
    /// gets a copy in an `alloca` and has to put it back.
    pub(super) fn recover_native_state_alloca(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<(LLVMValueRef, bool), LlvmError> {
        if self.state_is_boxed() {
            return Ok((self.native_state_payload_of_local(slot, type_id)?, false));
        }
        let value = self.load_native_state_local(slot, type_id, ty)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        let pointer = self.alloca(llvm_type, c"native.view.value");
        // SAFETY: `pointer` is a fresh alloca of `llvm_type` and `value` has it.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok((pointer, true))
    }

    /// Puts a recovered copy back, for the form that needs it.
    ///
    /// A no-op for a boxed state, which was never copied out.
    pub(super) fn write_back_native_state(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
        ty: Type,
        root: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        if self.state_is_boxed() {
            return Ok(());
        }
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: `root` is a live alloca of `llvm_type`.
        let updated = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                llvm_type,
                root,
                c"native.updated".as_ptr(),
            )
        };
        self.replace_native_state_local(slot, type_id, ty, updated)
    }

    /// The payload address for the token held in local `slot`.
    fn native_state_payload_of_local(
        &mut self,
        slot: u32,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let token = self.native_state_token_of_local(slot)?;
        self.native_state_payload(token, type_id)
    }

    /// The token word held in local `slot`.
    fn native_state_token_of_local(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let pointer = self.local_pointer(slot)?;
        // SAFETY: a recovered-view local is allocated as an i64 token slot.
        Ok(unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                self.codegen.types.i64,
                pointer,
                c"native.token".as_ptr(),
            )
        })
    }

    fn materialize_native_state(
        &mut self,
        token: LLVMValueRef,
        type_id: NativeStateTypeId,
        ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        if !self.state_is_boxed() {
            let node = self.recover_native_node(token, type_id)?;
            return self.codegen.decode_native_state_value(node, ty);
        }
        let payload = self.native_state_payload(token, type_id)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: the payload addresses an initialized value of `llvm_type`.
        let value = unsafe {
            LLVMBuildLoad2(
                self.codegen.builder,
                llvm_type,
                payload,
                c"native.state.value".as_ptr(),
            )
        };
        // The state keeps its own value, so a reader gets a copy of it — the
        // same rule every other read of an owned value follows. Only a whole
        // value goes through here; a field read walks the payload instead and
        // copies just the field.
        self.copy_value(value, ty)
    }

    /// The address of the value inside the box `token` names, type-checked.
    pub(super) fn native_state_payload(
        &mut self,
        token: LLVMValueRef,
        type_id: NativeStateTypeId,
    ) -> Result<LLVMValueRef, LlvmError> {
        let out = self.alloca(self.codegen.types.ptr, c"native.state.payload.out");
        let status = self.call(
            self.codegen.runtime.native_state_box_payload,
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
                c"native.state.payload".as_ptr(),
            )
        })
    }

    /// Stores `value` at `pointer`, dropping whatever was there.
    fn store_through_payload(
        &mut self,
        pointer: LLVMValueRef,
        ty: Type,
        value: LLVMValueRef,
    ) -> Result<(), LlvmError> {
        if self.codegen.owns_heap(ty) {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `pointer` addresses an initialized value of `llvm_type`.
            let old = unsafe {
                LLVMBuildLoad2(
                    self.codegen.builder,
                    llvm_type,
                    pointer,
                    c"native.state.old".as_ptr(),
                )
            };
            // SAFETY: same location, and `value` has that type.
            unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
            return self.drop_value(old, ty);
        }
        // SAFETY: `pointer` addresses a value of `ty` and `value` has its type.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok(())
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
