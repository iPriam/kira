//! Lowering a call to a `@FFI.Syscall` import: register words in, one
//! instruction, the kernel's answer out.
//!
//! This is the whole of what makes a Kira program able to be PID 1. Every other
//! way out of Kira goes through something that has to be found first — a symbol
//! in a shared object, an adapter in a sidecar, libffi's own archive — and in an
//! initramfs with no dynamic loader there is nothing to find. Here the call *is*
//! the code: [`super::super::syscall`] builds the inline-assembly callee, this
//! puts the arguments in it and reads the result back.
//!
//! # Why the arguments round-trip through their C width
//!
//! Kira holds every integer width and every pointer as an `i64` already, so an
//! argument could simply be handed over as it stands. It is narrowed to the
//! width the declaration wrote and widened back instead, because that is what
//! the register would have held had the same declaration been an `@FFI.Extern`.
//! Two answers to "what does `fd: I32` put in the register" is one too many: a
//! value outside `i32` would reach the kernel intact through one annotation and
//! truncated through the other.

use kira_ir::IrExprId;
use kira_runtime_abi::{ForeignAdapterStatus, ForeignType, ForeignTypeSpec};
use kira_semantics_model::Type;
use llvm_sys::LLVMIntPredicate;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::FunctionLowering;
use crate::LlvmError;

impl FunctionLowering<'_, '_> {
    /// Lowers a call to the syscall import at `index`.
    ///
    /// Reached from [`super::foreign::FunctionLowering::lower_foreign_call`],
    /// which dispatches on the import's ABI: the two forms share a callee kind,
    /// an arity check, and a result type, and differ only in how the call is
    /// reached.
    pub(super) fn lower_syscall_call(
        &mut self,
        index: u32,
        args: &[IrExprId],
        result_ty: Type,
    ) -> Result<LLVMValueRef, LlvmError> {
        let import = self.codegen.program.foreign_imports[index as usize]
            .import
            .clone();
        let syscall = import
            .as_syscall()
            .ok_or(LlvmError::internal("a syscall import with no known call"))?;
        let arch = self.codegen.syscall_arch().ok_or(LlvmError::internal(
            "a syscall reached a target with no kernel entry sequence",
        ))?;
        let params = import.signature().parameters().to_vec();
        let result_spec = import.signature().result();

        // Arguments evaluate left to right, as the VM pushes them.
        let mut values = Vec::with_capacity(args.len());
        for &argument in args {
            let ty = self.type_of(argument);
            values.push((self.lower_expr(argument)?, ty));
        }

        // The call number leads, because that is the operand order the constraint
        // string names.
        let mut call_args = Vec::with_capacity(values.len() + 1);
        // SAFETY: `i64` belongs to this live module's context.
        call_args.push(unsafe {
            LLVMConstInt(
                self.codegen.types.i64,
                syscall.number(arch) as u64,
                // Signed: every number in the table is positive, and saying so
                // keeps the constant from being read as an unsigned bit pattern
                // if one ever is not.
                1,
            )
        });
        let mut cstring_pointers = Vec::new();
        for ((value, _), spec) in values.iter().copied().zip(params.iter().copied()) {
            let word = match spec {
                ForeignTypeSpec::Scalar(ForeignType::CString) => {
                    let pointer = self.materialize_syscall_cstring(value);
                    cstring_pointers.push(pointer);
                    // SAFETY: `pointer` is the transient C string just built and
                    // `i64` belongs to this module's context.
                    unsafe {
                        LLVMBuildPtrToInt(
                            self.codegen.builder,
                            pointer,
                            self.codegen.types.i64,
                            c"syscall.arg.ptr".as_ptr(),
                        )
                    }
                }
                ForeignTypeSpec::Scalar(scalar) => self.syscall_register_word(value, scalar)?,
                ForeignTypeSpec::Aggregate(_) => {
                    return Err(LlvmError::internal(
                        "an aggregate reached a system-call argument",
                    ));
                }
            };
            call_args.push(word);
        }

        let callee = self.codegen.syscall_callee(arch, args.len());
        // SAFETY: `callee` was built for exactly this arity, so its type and the
        // argument list agree, and the builder is positioned in this function.
        let raw = unsafe {
            LLVMBuildCall2(
                self.codegen.builder,
                callee.ty,
                callee.value,
                call_args.as_mut_ptr(),
                call_args.len() as u32,
                c"syscall.result".as_ptr(),
            )
        };

        // The call owns the values its argument expressions produced. Freed after
        // the call and before the transient C strings, exactly as the C seam does
        // it: the kernel has read the buffer by the time it returns.
        for (value, ty) in values {
            self.drop_value(value, ty)?;
        }
        for pointer in cstring_pointers {
            self.call(
                self.codegen.runtime.cstring_free,
                &mut [pointer],
                c"syscall.cstring.free",
            );
        }

        if !syscall.returns() {
            return self.end_control_after_syscall(result_ty);
        }
        match result_spec {
            // A declaration that named no result discards the register. The
            // placeholder is the same one the C seam's void result produces.
            ForeignTypeSpec::Scalar(ForeignType::Void) => {
                // SAFETY: the type belongs to this live module context.
                Ok(unsafe { LLVMConstInt(self.codegen.types.i1, 0, 0) })
            }
            ForeignTypeSpec::Scalar(scalar) => {
                let narrowed = self.codegen.foreign_arg_to_c(raw, scalar)?;
                self.codegen.c_value_to_kira(narrowed, scalar)
            }
            ForeignTypeSpec::Aggregate(_) => Err(LlvmError::internal(
                "an aggregate reached a system-call result",
            )),
        }
    }

    /// Terminates the block after a call the kernel never returns from, and
    /// leaves the builder somewhere the rest of the body can still be lowered.
    ///
    /// `exit_group` is the only such call, and telling LLVM so is what spares the
    /// author the code that follows it. A Kira function has to end in a `return`
    /// and a caller that ends by exiting the process still has to write one; left
    /// unmarked, that return is emitted, reached, and the process carries on
    /// running past the point it was told to stop. Marked, everything after the
    /// call is dead and the optimizer drops it.
    fn end_control_after_syscall(&mut self, result_ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let function = self.current_function();
        let unreached = self.append_block(function, c"syscall.noreturn");
        // SAFETY: control does not come back from the instruction just emitted;
        // `unreached` belongs to this function, and the undef value stands in for
        // a result nothing can observe.
        let undef = unsafe {
            LLVMBuildUnreachable(self.codegen.builder);
            LLVMGetUndef(self.codegen.llvm_type(result_ty)?)
        };
        self.position_at(unreached);
        Ok(undef)
    }

    /// The full register word a scalar argument occupies.
    ///
    /// Narrowed to the declared width and widened back by the declared
    /// signedness, which is the round trip the module doc explains: `fd: I32`
    /// puts a sign-extended 32-bit value in the register whether it was declared
    /// with `@FFI.Syscall` or `@FFI.Extern`.
    fn syscall_register_word(
        &mut self,
        value: LLVMValueRef,
        scalar: ForeignType,
    ) -> Result<LLVMValueRef, LlvmError> {
        if scalar == ForeignType::Void {
            return Err(LlvmError::internal(
                "a `Void` reached a system-call argument",
            ));
        }
        let narrowed = self.codegen.kira_value_to_c(value, scalar)?;
        self.codegen.c_value_to_kira(narrowed, scalar)
    }

    /// Builds the transient NUL-terminated copy a `CString` argument passes, and
    /// traps when the Kira string cannot become one.
    ///
    /// A Kira `String` holds its length, so it may contain a NUL byte; a C string
    /// cannot. Passing one anyway would hand the kernel a path that silently ends
    /// at the first NUL — a `mount` of `/dev\0/sneaky` mounting `/dev` — so the
    /// same refusal the C seam raises is raised here.
    fn materialize_syscall_cstring(&mut self, value: LLVMValueRef) -> LLVMValueRef {
        let types = self.codegen.types;
        let builder = self.codegen.builder;
        let pointer = self.call(
            self.codegen.runtime.cstring_new,
            &mut [value],
            c"syscall.cstring.new",
        );
        // SAFETY: the builder is positioned in this function and both operands
        // are values of this module's pointer type.
        let is_null = unsafe {
            LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                pointer,
                LLVMConstPointerNull(types.ptr),
                c"syscall.cstring.null".as_ptr(),
            )
        };
        let function = self.current_function();
        let bad = self.append_block(function, c"syscall.cstring.bad");
        let good = self.append_block(function, c"syscall.cstring.good");
        // SAFETY: both blocks belong to the active function and the current block
        // is unterminated.
        unsafe { LLVMBuildCondBr(builder, is_null, bad, good) };
        self.position_at(bad);
        // SAFETY: the integer type belongs to this live module.
        let status = unsafe {
            LLVMConstInt(
                types.i32,
                u64::from(ForeignAdapterStatus::INTERIOR_NUL.0),
                0,
            )
        };
        self.call(self.codegen.runtime.trap_foreign, &mut [status], c"");
        // SAFETY: the foreign trap never returns.
        unsafe { LLVMBuildUnreachable(builder) };
        self.position_at(good);
        pointer
    }
}
