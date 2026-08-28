//! Call lowering: `print`, direct calls within this half, and the crossing into
//! the VM half of a hybrid program.

use kira_ir::{IrCallee, IrExprId, IrWriteback};
use kira_runtime_abi::NativeStateTypeId;
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::Callable;
use super::FunctionLowering;
use crate::LlvmError;

/// One lent argument evaluated into storage of its own.
struct LentTemporary {
    /// The slot the callee borrowed.
    pointer: LLVMValueRef,
    /// The value's type, for releasing what the slot holds.
    ty: Type,
    /// The stack pointer saved before the slot was allocated.
    saved_stack: LLVMValueRef,
}

impl FunctionLowering<'_, '_> {
    /// Lowers a call to `print` or a user function.
    ///
    /// `writebacks` is non-empty only for a call whose callee writes through one
    /// or more parameters — a mutating method's receiver, or a `borrow mut`
    /// parameter. Each names the place the write must land in; that case passes
    /// the parameter by pointer into its place, so the callee's writes are the
    /// caller's, and the call still yields the declared result.
    pub(super) fn lower_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
        writebacks: &[IrWriteback],
        result_ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        match callee {
            IrCallee::Print => {
                let argument = *args
                    .first()
                    .ok_or(LlvmError::internal("a print with no argument"))?;
                let ty = self.type_of(argument);
                let mut value = self.lower_expr(argument)?;
                let helper = match ty {
                    // Analysis rejects printing a struct, an array, or an enum —
                    // none has a rendering the language pins — so a program that
                    // type-checked never reaches any of these arms.
                    Type::Array(_) => {
                        return Err(LlvmError::internal("a print of an array"));
                    }
                    Type::Enum(_) => {
                        return Err(LlvmError::internal("a print of an enum"));
                    }
                    Type::Int(_) => self.codegen.runtime.print_int,
                    Type::Float(_) => self.codegen.runtime.print_float,
                    Type::Bool => {
                        // Booleans are `i1` in registers but cross the C ABI as
                        // a byte.
                        // SAFETY: `value` is an `i1` and the builder is live.
                        value = unsafe {
                            LLVMBuildZExt(
                                self.codegen.builder,
                                value,
                                self.codegen.types.i8,
                                c"bool.byte".as_ptr(),
                            )
                        };
                        self.codegen.runtime.print_bool
                    }
                    // `print` consumes its string, so the helper frees it.
                    Type::String => self.codegen.runtime.print_str,
                    // Analysis rejects printing a struct — what it renders is
                    // not pinned by the language — so this is unreachable from
                    // a program that type-checked.
                    Type::Struct(_) => {
                        return Err(LlvmError::internal("a print of a struct"));
                    }
                    // Analysis rejects `print` of a raw pointer (an opaque
                    // foreign word has no pinned rendering) and `CString` is
                    // seam-only, so neither reaches a type-checked program.
                    Type::RawPtr
                    | Type::ForeignPtr(_)
                    | Type::CString
                    | Type::CBlock
                    | Type::NativeState(_)
                    | Type::Task(_)
                    | Type::Cell(_) => {
                        return Err(LlvmError::internal("a print of a raw pointer"));
                    }
                    // Analysis rejects `print` of an erased value for the same
                    // reason it rejects a struct or an array: no rendering is
                    // pinned, and here there is not even a type to pin one for.
                    Type::Any => {
                        return Err(LlvmError::internal("a print of an erased value"));
                    }
                    Type::Void | Type::Error => {
                        return Err(LlvmError::internal("printing a value with no type"));
                    }
                };
                Ok(self.call(helper, &mut [value], c""))
            }
            IrCallee::User(index) => {
                let target = *self
                    .codegen
                    .functions
                    .get(index as usize)
                    .ok_or(LlvmError::internal("a call to an unknown function"))?;
                // A written-through parameter is taken by reference: the caller
                // hands over a pointer into its place, so the callee's writes
                // land in the caller's storage.
                if !writebacks.is_empty() {
                    return match target {
                        Some(_) => self.lower_writeback_call(index, writebacks, args),
                        // The callee is on the VM, where a pointer into this
                        // half's storage means nothing. So the value crosses as
                        // a copy and comes back as one.
                        None => {
                            // Arguments evaluate left to right, as the VM pushes
                            // them; a written-through position is lowered like
                            // any other, because what crosses is its value.
                            let mut values = Vec::with_capacity(args.len());
                            for &argument in args {
                                values.push(self.lower_expr(argument)?);
                            }
                            self.lower_runtime_call_writing_back(index, args, &values, writebacks)
                        }
                    };
                }
                match target {
                    // The callee is in this half: an ordinary direct call.
                    Some(target) => self.lower_direct_call(index, target, args),
                    // The callee runs on the VM: marshal and go through the
                    // bridge, which the host answers.
                    None => {
                        // Arguments evaluate left to right, as the VM pushes them.
                        let mut values = Vec::with_capacity(args.len());
                        for &argument in args {
                            values.push(self.lower_expr(argument)?);
                        }
                        self.lower_runtime_call(index, args, &values)
                    }
                }
            }
            // A foreign C function: marshal the arguments to the import's
            // exact-width signature and invoke the generated adapter directly.
            IrCallee::Foreign(index) => self.lower_foreign_call(index, args, result_ty),
        }
    }

    /// Lowers a call to a function compiled in this module.
    ///
    /// A parameter the callee takes by pointer is *lent* its argument: when the
    /// argument names a place, its address goes over and nothing is copied —
    /// which is the whole point, since the values worth lending are the ones
    /// expensive to copy. An argument that is not a place is evaluated into a
    /// temporary the caller owns, lent from there, and dropped after the call,
    /// so the callee sees the same value either way.
    fn lower_direct_call(
        &mut self,
        index: u32,
        target: Callable,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let callee = &self.codegen.program.functions[index as usize];
        let lends: Vec<bool> = (0..args.len())
            .map(|position| {
                self.codegen.param_is_pointer(callee, position as u32)
                    && !callee.param_by_reference(position as u32)
            })
            .collect();
        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len());
        let mut temporaries = Vec::new();
        for (position, &argument) in args.iter().enumerate() {
            if lends.get(position).copied().unwrap_or(false) {
                let (pointer, temporary) = self.lend_argument(argument)?;
                if let Some(temporary) = temporary {
                    temporaries.push(temporary);
                }
                values.push(pointer);
                continue;
            }
            // A borrow this module did not lend still arrives as a copy the
            // callee owns, and the caller keeps the value it was read from.
            let borrowed = callee.param_by_pointer(position as u32)
                || callee.param_by_reference(position as u32);
            values.push(match borrowed {
                true => self.lower_borrowed_expr(argument)?,
                false => self.lower_expr(argument)?,
            });
        }
        let returns_value =
            self.codegen.program.functions[index as usize].return_type != Type::Void;
        let name = if returns_value { c"call" } else { c"" };
        let result = self.call(target, &mut values, name);
        self.drop_lent_temporaries(temporaries)?;
        Ok(result)
    }

    /// Releases the temporaries a call's lent arguments needed.
    ///
    /// The callee borrowed them and owns none, so the caller that built them
    /// drops them once the call is over — and gives back the native stack
    /// each dynamic alloca reserved, which is what keeps a lent argument in a
    /// loop from walking the stack to its limit.
    ///
    /// In **reverse** order, because the restores are stack-pointer restores
    /// and therefore nest: each temporary's save was taken while every earlier
    /// one was already allocated, so restoring the oldest first would pop the
    /// younger slots while their values still wait to be released — and the
    /// release calls that follow would scribble over exactly those bytes.
    fn drop_lent_temporaries(&mut self, temporaries: Vec<LentTemporary>) -> Result<(), LlvmError> {
        for temporary in temporaries.into_iter().rev() {
            // Through the temporary's own storage: it is dead after this, so
            // loading it only to hand the value back through memory is work
            // with nobody to read it.
            self.codegen.release_at(temporary.pointer, temporary.ty)?;
            self.codegen
                .release_dynamic_alloca(temporary.pointer, temporary.saved_stack);
        }
        Ok(())
    }

    /// The address to lend for one argument, and the temporary it needed.
    ///
    /// A place is lent where it lies. Anything else is evaluated once into a
    /// fresh slot, whose value the caller still owns — so the second element
    /// names it for the drop that follows the call, and the third carries the
    /// stack pointer saved for its dynamic alloca, restored by that same drop.
    fn lend_argument(
        &mut self,
        argument: IrExprId,
    ) -> Result<(LLVMValueRef, Option<LentTemporary>), LlvmError> {
        if let Some((pointer, _)) = self.borrowed_place_pointer(argument)? {
            return Ok((pointer, None));
        }
        let ty = self.type_of(argument);
        let value = self.lower_expr(argument)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        // SAFETY: the builder is on a live block and `llvm_type` is this
        // module's.
        let (pointer, saved) = self.codegen.dynamic_alloca(llvm_type, c"lent.arg");
        self.codegen.lifetime_start(pointer);
        // SAFETY: a fresh alloca of exactly this value's type.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok((
            pointer,
            Some(LentTemporary {
                pointer,
                ty,
                saved_stack: saved,
            }),
        ))
    }

    /// Lowers a call whose callee writes through one or more of its parameters.
    ///
    /// Each written-through parameter is passed as a pointer into its place,
    /// walked exactly as `append` walks to the array it grows; the callee is
    /// compiled with a pointer in that position (see `declare_function`) and
    /// mutates through it, so the write is the caller's. The remaining arguments
    /// are ordinary by-value arguments, evaluated left to right. The call still
    /// returns the callee's declared result. Only ever a same-half call — a
    /// written-through value has no seam representation — so `target` is
    /// resolved in this module.
    ///
    /// A place rooted at a recovered native-state local is addressable like any
    /// other: the token names a box holding the value, so the walk starts at
    /// that box's payload and the callee writes straight into the state.
    fn lower_writeback_call(
        &mut self,
        index: u32,
        writebacks: &[IrWriteback],
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        let target = self
            .codegen
            .functions
            .get(index as usize)
            .copied()
            .flatten()
            .ok_or(LlvmError::internal(
                "a writeback call to a function not in this half",
            ))?;
        let mut pointers: Vec<(u32, LLVMValueRef)> = Vec::with_capacity(writebacks.len());
        // Native-state roots that were copied out and owe a write-back. Empty
        // whenever state is boxed, because then the callee wrote into it.
        let mut recovered: Vec<(u32, NativeStateTypeId, Type, LLVMValueRef)> = Vec::new();
        for writeback in writebacks {
            let place = &writeback.place;
            let pointer = if let Some(type_id) = self
                .function
                .native_state_locals
                .get(place.local as usize)
                .copied()
                .flatten()
            {
                let root_ty = self.local_type(place.local)?;
                // For a boxed state the callee writes through this pointer into
                // the state's own storage, so the write is already where it
                // belongs when the call returns.
                let (root, write_back) =
                    self.recover_native_state_alloca(place.local, type_id, root_ty)?;
                let mut pointer = root;
                let mut pointee = root_ty;
                for step in &place.path {
                    (pointer, pointee) = self.walk_place_step(pointer, pointee, step)?;
                }
                if write_back {
                    recovered.push((place.local, type_id, root_ty, root));
                }
                pointer
            } else {
                self.walk_place(place.local, &place.path)?.0
            };
            pointers.push((writeback.param, pointer));
        }
        let mut values = Vec::with_capacity(args.len());
        let mut temporaries = Vec::new();
        for (position, &argument) in args.iter().enumerate() {
            match pointers
                .iter()
                .find(|(param, _)| *param as usize == position)
            {
                // A written-through position takes the pointer rather than the
                // argument's value: the callee reads its current contents
                // through it, so lowering the expression as well would compute
                // the same value twice and discard one.
                Some((_, pointer)) => values.push(*pointer),
                // Every other position follows the callee's signature just as a
                // call with no write-backs does: a lent parameter takes an
                // address, everything else its value.
                None => {
                    let callee = &self.codegen.program.functions[index as usize];
                    let lends = self.codegen.param_is_pointer(callee, position as u32)
                        && !callee.param_by_reference(position as u32);
                    if lends {
                        let (pointer, temporary) = self.lend_argument(argument)?;
                        if let Some(temporary) = temporary {
                            temporaries.push(temporary);
                        }
                        values.push(pointer);
                    } else {
                        values.push(self.lower_expr(argument)?);
                    }
                }
            }
        }
        let returns_value =
            self.codegen.program.functions[index as usize].return_type != Type::Void;
        let name = if returns_value { c"call" } else { c"" };
        let result = self.call(target, &mut values, name);
        for (local, type_id, root_ty, root) in recovered {
            self.write_back_native_state(local, type_id, root_ty, root)?;
        }
        self.drop_lent_temporaries(temporaries)?;
        Ok(result)
    }

    /// Calls a function that lives in the VM half, from native code.
    ///
    /// The mirror of the VM's `CallNative`: arguments are packed into a stack
    /// array of `BridgeValue`s, `kira_hybrid_call_runtime` hands them to the
    /// host's invoker, and the result is unpacked. The array is an `alloca`, so
    /// a crossing costs no heap allocation on this side either.
    pub(super) fn lower_runtime_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
        values: &[LLVMValueRef],
    ) -> Result<LLVMValueRef, LlvmError> {
        self.lower_runtime_call_writing_back(index, args, values, &[])
    }

    /// [`Self::lower_runtime_call`], storing each written-through parameter's
    /// final value back into the caller's place.
    ///
    /// A pointer cannot cross: the VM half holds its values in a heap this side
    /// has no address in. So a `borrow mut` goes over as a copy like any other
    /// argument, the invoker packs the callee's final value back into the slot
    /// it arrived in, and this reads it out and stores it — dropping what the
    /// place held, exactly as an assignment does.
    fn lower_runtime_call_writing_back(
        &mut self,
        index: u32,
        args: &[IrExprId],
        values: &[LLVMValueRef],
        writebacks: &[IrWriteback],
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        let result_type = self.codegen.program.functions[index as usize].return_type;

        // Given back as soon as the call returns — see the foreign path for
        // why neither leaving it reserved nor hoisting it to the entry block
        // works.
        let saved = self.call(self.codegen.runtime.stack_save, &mut [], c"stack.save");
        // SAFETY: every type and value belongs to this live module and the
        // builder is on a live block; the argument array is sized to hold
        // exactly the arguments written into it.
        let out = unsafe {
            let count = LLVMConstInt(types.i64, values.len() as u64, 0);
            let argv =
                LLVMBuildArrayAlloca(builder, types.bridge_value, count, c"bridge.args".as_ptr());
            for (slot, (&value, &expr)) in values.iter().zip(args).enumerate() {
                let mut offset = [LLVMConstInt(types.i32, slot as u64, 0)];
                let element = LLVMBuildInBoundsGEP2(
                    builder,
                    types.bridge_value,
                    argv,
                    offset.as_mut_ptr(),
                    1,
                    c"bridge.arg".as_ptr(),
                );
                self.codegen
                    .write_bridge_value(element, value, self.type_of(expr))?;
            }

            let out = LLVMBuildAlloca(builder, types.bridge_value, c"bridge.out".as_ptr());
            let mut call_args = [
                LLVMConstInt(types.i32, u64::from(index), 0),
                argv,
                LLVMConstInt(types.i32, values.len() as u64, 0),
                out,
            ];
            self.codegen
                .call_runtime(self.codegen.runtime.call_runtime, &mut call_args, c"");
            (out, argv)
        };
        let (out, argv) = out;
        // Read the written-through slots before the stack goes back, for the
        // same reason the result is read here: `argv` is on it.
        for writeback in writebacks {
            let param_ty = self
                .codegen
                .program
                .functions
                .get(index as usize)
                .and_then(|callee| callee.param_type(writeback.param))
                .ok_or(LlvmError::internal(
                    "a writeback naming a parameter the callee does not have",
                ))?;
            // SAFETY: `argv` holds `values.len()` slots, and a writeback's
            // parameter is one of the callee's — which is that many.
            let element = unsafe {
                let mut offset = [LLVMConstInt(types.i32, u64::from(writeback.param), 0)];
                LLVMBuildInBoundsGEP2(
                    builder,
                    types.bridge_value,
                    argv,
                    offset.as_mut_ptr(),
                    1,
                    c"bridge.writeback".as_ptr(),
                )
            };
            let returned = self.codegen.read_bridge_payload(element, param_ty)?;
            let place = &writeback.place;
            let (pointer, pointee) = self.walk_place(place.local, &place.path)?;
            self.store_through(pointer, pointee, returned)?;
        }
        // Read the payload before giving the stack back: `out` is on it.
        let value = self
            .codegen
            .read_bridge_payload(out, result_type)
            .or_else(|error| {
                // A `Void` callee returns nothing to read; anything else is a real
                // failure.
                if result_type == Type::Void {
                    // SAFETY: `i1 false` is a placeholder no caller of a Void call
                    // ever consumes; `Eval` discards it and nothing else can name it.
                    Ok(unsafe { LLVMConstInt(types.i1, 0, 0) })
                } else {
                    Err(error)
                }
            })?;
        self.call(self.codegen.runtime.stack_restore, &mut [saved], c"");
        Ok(value)
    }
}
