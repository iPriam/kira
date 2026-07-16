//! Statement and expression lowering: the part that must agree with the
//! interpreter instruction for instruction.
//!
//! Where LLVM's natural choice differs from the VM's, the VM wins:
//!
//! - `add`/`sub`/`mul` carry no `nsw`/`nuw`, so they wrap like `wrapping_add`,
//! - `/` and `%` test their divisor and call the runtime trap on zero, and
//!   special-case `MIN / -1` (which is poison in LLVM but a defined wrapping
//!   result in the VM),
//! - a local read *clones* its string and every consuming operation frees one,
//!   mirroring the VM's affine string heap — so a native run reclaims exactly
//!   what an interpreted run does.

use kira_ir::{IrBinOp, IrCallee, IrExpr, IrExprId, IrFunction, IrStmt, IrUnOp};
use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;
use llvm_sys::{LLVMIntPredicate, LLVMLinkage, LLVMRealPredicate, LLVMUnnamedAddr};

use super::{Callable, Codegen, c_string};
use crate::LlvmError;

impl<'a> Codegen<'a> {
    /// Lowers one Kira function body.
    pub(super) fn lower_function(
        &mut self,
        index: usize,
        function: &'a IrFunction,
    ) -> Result<(), LlvmError> {
        let value = self.functions[index].value;

        // SAFETY: `value` is a function in this live module; the builder is
        // positioned on its entry block before any instruction is built.
        unsafe {
            let entry = LLVMAppendBasicBlockInContext(self.context, value, c"entry".as_ptr());
            LLVMPositionBuilderAtEnd(self.builder, entry);
        }

        let locals = self.allocate_locals(function, value)?;
        let mut body = FunctionLowering {
            codegen: self,
            function,
            locals,
        };
        body.lower_block(&function.body)?;
        body.finish()
    }

    /// Allocates one stack slot per local and initializes every one.
    ///
    /// Slots start at their type's zero (`0`, `0.0`, `false`, the null string
    /// handle), mirroring the VM initializing every slot to `Void`: a `let` in
    /// a loop body then frees the previous iteration's string through the same
    /// path an assignment does, with no special case for the first store.
    fn allocate_locals(
        &mut self,
        function: &IrFunction,
        value: LLVMValueRef,
    ) -> Result<Vec<LLVMValueRef>, LlvmError> {
        let mut locals = Vec::with_capacity(function.locals.len());
        for (slot, &ty) in function.locals.iter().enumerate() {
            let llvm_type = self.llvm_type(ty)?;
            let name = c_string(&format!("local.{slot}"));
            // SAFETY: the builder sits on the function's entry block, and every
            // type and value below comes from this module's context.
            unsafe {
                let alloca = LLVMBuildAlloca(self.builder, llvm_type, name.as_ptr());
                let initial = if (slot as u32) < function.param_count {
                    // Parameters take ownership of the caller's argument, just
                    // as the VM moves arguments into the callee's slots.
                    LLVMGetParam(value, slot as u32)
                } else {
                    self.zero_value(ty)?
                };
                LLVMBuildStore(self.builder, initial, alloca);
                locals.push(alloca);
            }
        }
        Ok(locals)
    }

    /// An `Int` constant.
    ///
    /// Constants are module-level values, so unlike instructions they need no
    /// builder position — only types from this context.
    pub(super) fn const_int(&self, value: i64) -> LLVMValueRef {
        // SAFETY: `i64` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.i64, value as u64, 1) }
    }

    /// A `Float` constant.
    pub(super) fn const_float(&self, value: f64) -> LLVMValueRef {
        // SAFETY: `f64` belongs to this module's live context.
        unsafe { LLVMConstReal(self.types.f64, value) }
    }

    /// A `Bool` constant.
    pub(super) fn const_bool(&self, value: bool) -> LLVMValueRef {
        // SAFETY: `i1` belongs to this module's live context.
        unsafe { LLVMConstInt(self.types.i1, u64::from(value), 0) }
    }

    /// The zero value a fresh local slot holds.
    fn zero_value(&self, ty: Type) -> Result<LLVMValueRef, LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: `llvm_type` belongs to this module's context.
        Ok(unsafe {
            match ty {
                Type::Int | Type::Bool => LLVMConstInt(llvm_type, 0, 0),
                Type::Float => LLVMConstReal(llvm_type, 0.0),
                Type::String => LLVMConstPointerNull(llvm_type),
                Type::Void | Type::Error => {
                    return Err(LlvmError::Unsupported("a local with no runtime value"));
                }
            }
        })
    }

    /// Builds a private constant global holding `text`, returning a pointer to
    /// its bytes (the null pointer for the empty string, which never
    /// allocates).
    fn string_constant(&mut self, text: &str) -> LLVMValueRef {
        let bytes = text.as_bytes();
        // SAFETY: every type and value below is from this live module; `bytes`
        // outlives the constant-array copy LLVM makes.
        unsafe {
            if bytes.is_empty() {
                return LLVMConstPointerNull(self.types.ptr);
            }
            let name = c_string(&format!("kira.str.{}", self.string_counter));
            self.string_counter += 1;
            let initializer = LLVMConstStringInContext2(
                self.context,
                bytes.as_ptr().cast(),
                bytes.len(),
                1, // Kira strings carry their length; no NUL terminator.
            );
            let array = LLVMArrayType2(self.types.i8, bytes.len() as u64);
            let global = LLVMAddGlobal(self.module, array, name.as_ptr());
            LLVMSetInitializer(global, initializer);
            LLVMSetGlobalConstant(global, 1);
            LLVMSetLinkage(global, LLVMLinkage::LLVMPrivateLinkage);
            // Identical literals may share storage: the runtime copies out of
            // them and never compares their addresses.
            LLVMSetUnnamedAddress(global, LLVMUnnamedAddr::LLVMGlobalUnnamedAddr);
            global
        }
    }
}

/// Lowering state for one function body.
struct FunctionLowering<'a, 'p> {
    codegen: &'a mut Codegen<'p>,
    function: &'p IrFunction,
    /// One `alloca` per local slot, in slot order.
    locals: Vec<LLVMValueRef>,
}

impl FunctionLowering<'_, '_> {
    /// Terminates the body when control can still fall off its end.
    ///
    /// A `Void` function returns unit, mirroring the bytecode compiler's
    /// trailing `ReturnVoid`. For a value-returning function the analyzer has
    /// already proved every path returns, so the fall-through is unreachable —
    /// and saying so lets LLVM keep the guarantee rather than inventing a value.
    fn finish(&mut self) -> Result<(), LlvmError> {
        if self.block_is_terminated() {
            return Ok(());
        }
        if self.function.return_type == Type::Void {
            self.emit_return(None)
        } else {
            // SAFETY: the builder is positioned on an unterminated block.
            unsafe { LLVMBuildUnreachable(self.codegen.builder) };
            Ok(())
        }
    }

    /// Lowers a statement list.
    fn lower_block(&mut self, statements: &[IrStmt]) -> Result<(), LlvmError> {
        for statement in statements {
            // Everything after a `return` is unreachable; the VM simply never
            // executes it, and emitting into a terminated block is invalid IR.
            if self.block_is_terminated() {
                break;
            }
            self.lower_statement(statement)?;
        }
        Ok(())
    }

    /// Lowers one statement.
    fn lower_statement(&mut self, statement: &IrStmt) -> Result<(), LlvmError> {
        match statement {
            // `let` and `=` are the same store: the VM compiles both to
            // StoreLocal, which drops whatever the slot held.
            IrStmt::Let { local, init } => self.store_local(*local, *init),
            IrStmt::Assign { local, value } => self.store_local(*local, *value),
            IrStmt::Return { value } => {
                let returned = match value {
                    Some(expr) => Some(self.lower_expr(*expr)?),
                    None => None,
                };
                self.emit_return(returned)
            }
            IrStmt::Eval { expr } => {
                let value = self.lower_expr(*expr)?;
                // The VM's `Pop` drops the discarded value; only a string owns
                // anything to reclaim.
                if self.type_of(*expr) == Type::String {
                    self.free_string(value);
                }
                Ok(())
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self.lower_if(*cond, then_body, else_body),
            IrStmt::While { cond, body } => self.lower_while(*cond, body),
        }
    }

    /// Stores into a local slot, freeing the value it held.
    ///
    /// The new value is computed *before* the old one is freed, matching the
    /// VM's evaluate-then-StoreLocal order — which is what makes `s = s + "x"`
    /// work: the read clones `s` before the store frees the original.
    fn store_local(&mut self, slot: u32, expr: IrExprId) -> Result<(), LlvmError> {
        let value = self.lower_expr(expr)?;
        let ty = self.local_type(slot)?;
        let pointer = self.local_pointer(slot)?;

        if ty == Type::String {
            let llvm_type = self.codegen.llvm_type(ty)?;
            // SAFETY: `pointer` is this slot's alloca of `llvm_type`, and the
            // builder is positioned on a live block.
            let old = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"old".as_ptr())
            };
            // SAFETY: same slot and a matching value type.
            unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
            self.free_string(old);
            return Ok(());
        }
        // SAFETY: `pointer` is this slot's alloca and `value` has its type.
        unsafe { LLVMBuildStore(self.codegen.builder, value, pointer) };
        Ok(())
    }

    /// Returns from the function, first reclaiming every string this frame owns.
    ///
    /// The VM drops a finished frame's locals after popping the result; because
    /// a local read clones, a returned string is never one of the slots being
    /// freed here.
    fn emit_return(&mut self, value: Option<LLVMValueRef>) -> Result<(), LlvmError> {
        for slot in 0..self.function.locals.len() as u32 {
            if self.local_type(slot)? != Type::String {
                continue;
            }
            let pointer = self.local_pointer(slot)?;
            let llvm_type = self.codegen.types.ptr;
            // SAFETY: `pointer` is a live alloca holding a string handle.
            let held = unsafe {
                LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, c"drop".as_ptr())
            };
            self.free_string(held);
        }
        // SAFETY: the builder is positioned on an unterminated block, and
        // `value` matches the function's return type when present.
        unsafe {
            match value {
                Some(value) => LLVMBuildRet(self.codegen.builder, value),
                None => LLVMBuildRetVoid(self.codegen.builder),
            };
        }
        Ok(())
    }

    /// Lowers `if`/`else` into blocks.
    fn lower_if(
        &mut self,
        cond: IrExprId,
        then_body: &[IrStmt],
        else_body: &[IrStmt],
    ) -> Result<(), LlvmError> {
        let condition = self.lower_expr(cond)?;
        let function = self.current_function();
        let (then_block, else_block, merge_block) = (
            self.append_block(function, c"if.then"),
            self.append_block(function, c"if.else"),
            self.append_block(function, c"if.end"),
        );

        // SAFETY: all three blocks belong to the function being built.
        unsafe { LLVMBuildCondBr(self.codegen.builder, condition, then_block, else_block) };

        for (block, body) in [(then_block, then_body), (else_block, else_body)] {
            self.position_at(block);
            self.lower_block(body)?;
            if !self.block_is_terminated() {
                // SAFETY: the arm fell through; join the continuation.
                unsafe { LLVMBuildBr(self.codegen.builder, merge_block) };
            }
        }
        self.position_at(merge_block);
        Ok(())
    }

    /// Lowers a pre-tested loop into blocks.
    fn lower_while(&mut self, cond: IrExprId, body: &[IrStmt]) -> Result<(), LlvmError> {
        let function = self.current_function();
        let (test_block, body_block, exit_block) = (
            self.append_block(function, c"while.test"),
            self.append_block(function, c"while.body"),
            self.append_block(function, c"while.end"),
        );

        // SAFETY: every block belongs to the function being built, and the
        // condition is re-evaluated on each iteration as the VM does.
        unsafe { LLVMBuildBr(self.codegen.builder, test_block) };
        self.position_at(test_block);
        let condition = self.lower_expr(cond)?;
        // SAFETY: the test block is unterminated and `condition` is an `i1`.
        unsafe { LLVMBuildCondBr(self.codegen.builder, condition, body_block, exit_block) };

        self.position_at(body_block);
        self.lower_block(body)?;
        if !self.block_is_terminated() {
            // SAFETY: the body fell through; loop back to the test.
            unsafe { LLVMBuildBr(self.codegen.builder, test_block) };
        }
        self.position_at(exit_block);
        Ok(())
    }

    /// Lowers an expression to a value.
    fn lower_expr(&mut self, id: IrExprId) -> Result<LLVMValueRef, LlvmError> {
        match self.codegen.program.expr(id).clone() {
            IrExpr::Int(value) => Ok(self.codegen.const_int(value)),
            IrExpr::Float(value) => Ok(self.codegen.const_float(value)),
            IrExpr::Bool(value) => Ok(self.codegen.const_bool(value)),
            IrExpr::Str(text) => {
                let data = self.codegen.string_constant(&text);
                let length = self.codegen.const_int(text.len() as i64);
                Ok(self.call(self.codegen.runtime.str_new, &mut [data, length], c"str"))
            }
            IrExpr::Local(slot) => self.load_local(slot),
            IrExpr::Unary { op, operand } => {
                let value = self.lower_expr(operand)?;
                Ok(self.lower_unary(op, value))
            }
            IrExpr::Binary { op, lhs, rhs } => self.lower_binary(op, lhs, rhs),
            IrExpr::Call { callee, args, .. } => self.lower_call(callee, &args),
        }
    }

    /// Reads a local slot, cloning the string it holds.
    ///
    /// The VM's `LoadLocal` copies the value, so the slot keeps ownership of
    /// its own string and the reader owns an independent one.
    fn load_local(&mut self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        let ty = self.local_type(slot)?;
        let llvm_type = self.codegen.llvm_type(ty)?;
        let pointer = self.local_pointer(slot)?;
        let name = c_string(&format!("local.{slot}.read"));
        // SAFETY: `pointer` is this slot's alloca of `llvm_type`.
        let value =
            unsafe { LLVMBuildLoad2(self.codegen.builder, llvm_type, pointer, name.as_ptr()) };
        if ty == Type::String {
            return Ok(self.call(self.codegen.runtime.str_clone, &mut [value], c"str.copy"));
        }
        Ok(value)
    }

    /// Lowers a unary operator.
    fn lower_unary(&mut self, op: IrUnOp, value: LLVMValueRef) -> LLVMValueRef {
        let builder = self.codegen.builder;
        // SAFETY: `value` has the operand type the typed operator fixes, and
        // the builder is on a live block. `LLVMBuildNeg` carries no `nsw`, so
        // it wraps like the VM's `wrapping_neg`.
        unsafe {
            match op {
                IrUnOp::NegInt => LLVMBuildNeg(builder, value, c"neg".as_ptr()),
                IrUnOp::NegFloat => LLVMBuildFNeg(builder, value, c"fneg".as_ptr()),
                IrUnOp::Not => LLVMBuildNot(builder, value, c"not".as_ptr()),
            }
        }
    }

    /// Lowers a binary operator.
    fn lower_binary(
        &mut self,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<LLVMValueRef, LlvmError> {
        // Short-circuit operators are control flow, not instructions: the VM
        // never evaluates the right operand unless the left demands it.
        match op {
            IrBinOp::And => return self.lower_short_circuit(lhs, rhs, false),
            IrBinOp::Or => return self.lower_short_circuit(lhs, rhs, true),
            _ => {}
        }

        let left = self.lower_expr(lhs)?;
        let right = self.lower_expr(rhs)?;
        let builder = self.codegen.builder;

        // SAFETY: both operands carry the types the typed operator fixes, and the
        // builder is on a live block. None of the integer builders set
        // `nsw`/`nuw`, so they wrap as the VM does.
        let value = unsafe {
            match op {
                IrBinOp::AddInt => LLVMBuildAdd(builder, left, right, c"add".as_ptr()),
                IrBinOp::SubInt => LLVMBuildSub(builder, left, right, c"sub".as_ptr()),
                IrBinOp::MulInt => LLVMBuildMul(builder, left, right, c"mul".as_ptr()),
                IrBinOp::DivInt | IrBinOp::RemInt => {
                    return self.lower_int_division(op, left, right);
                }
                IrBinOp::AddFloat => LLVMBuildFAdd(builder, left, right, c"fadd".as_ptr()),
                IrBinOp::SubFloat => LLVMBuildFSub(builder, left, right, c"fsub".as_ptr()),
                IrBinOp::MulFloat => LLVMBuildFMul(builder, left, right, c"fmul".as_ptr()),
                IrBinOp::DivFloat => LLVMBuildFDiv(builder, left, right, c"fdiv".as_ptr()),
                IrBinOp::ConcatStr => {
                    return Ok(self.call(
                        self.codegen.runtime.str_concat,
                        &mut [left, right],
                        c"str.concat",
                    ));
                }
                IrBinOp::EqStr | IrBinOp::NeStr => {
                    return Ok(self.lower_string_compare(op, left, right));
                }
                other => {
                    let predicate = integer_predicate(other);
                    match predicate {
                        Some(predicate) => {
                            LLVMBuildICmp(builder, predicate, left, right, c"icmp".as_ptr())
                        }
                        None => {
                            let predicate = real_predicate(other).ok_or(LlvmError::Unsupported(
                                "an operator with no native lowering",
                            ))?;
                            LLVMBuildFCmp(builder, predicate, left, right, c"fcmp".as_ptr())
                        }
                    }
                }
            }
        };
        Ok(value)
    }

    /// Lowers `/` or `%` with the VM's exact semantics.
    ///
    /// Two cases LLVM would get wrong on its own: a zero divisor is a trap in
    /// Kira (not UB), and `MIN / -1` overflows — poison for `sdiv`, but a
    /// defined wrapping result for the VM's `wrapping_div`. Both are branched
    /// on explicitly, so the fast path stays a plain `sdiv`/`srem`.
    fn lower_int_division(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
    ) -> Result<LLVMValueRef, LlvmError> {
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        let function = self.current_function();
        let trap_block = self.append_block(function, c"div.trap");
        let overflow_block = self.append_block(function, c"div.overflow");
        let normal_block = self.append_block(function, c"div.normal");
        let done_block = self.append_block(function, c"div.done");

        // SAFETY: every block belongs to the function being built, both
        // operands are `i64`, and each block is terminated exactly once below.
        let (overflow_value, normal_value) = unsafe {
            let zero = LLVMConstInt(types.i64, 0, 0);
            let by_zero = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                right,
                zero,
                c"div.by.zero".as_ptr(),
            );
            LLVMBuildCondBr(builder, by_zero, trap_block, overflow_block);

            // Divisor is zero: the runtime reports the trap and exits, so
            // nothing follows.
            LLVMPositionBuilderAtEnd(builder, trap_block);
            self.codegen
                .call_runtime(self.codegen.runtime.trap_div_zero, &mut [], c"");
            LLVMBuildUnreachable(builder);

            // Divisor is -1: `MIN / -1` would be poison, so take the wrapping
            // answer directly. `x / -1` is `-x` and `x % -1` is 0 for every x,
            // so this branch needs no division at all.
            LLVMPositionBuilderAtEnd(builder, overflow_block);
            let minus_one = LLVMConstInt(types.i64, u64::MAX, 1);
            let by_minus_one = LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                right,
                minus_one,
                c"div.by.minus.one".as_ptr(),
            );
            let wrap_block = self.append_block(function, c"div.wrap");
            LLVMBuildCondBr(builder, by_minus_one, wrap_block, normal_block);

            LLVMPositionBuilderAtEnd(builder, wrap_block);
            let wrapped = match op {
                IrBinOp::DivInt => LLVMBuildNeg(builder, left, c"div.wrapped".as_ptr()),
                _ => zero,
            };
            LLVMBuildBr(builder, done_block);
            let wrap_exit = LLVMGetInsertBlock(builder);

            LLVMPositionBuilderAtEnd(builder, normal_block);
            let divided = match op {
                IrBinOp::DivInt => LLVMBuildSDiv(builder, left, right, c"div".as_ptr()),
                _ => LLVMBuildSRem(builder, left, right, c"rem".as_ptr()),
            };
            LLVMBuildBr(builder, done_block);
            let normal_exit = LLVMGetInsertBlock(builder);

            ((wrapped, wrap_exit), (divided, normal_exit))
        };

        self.position_at(done_block);
        // SAFETY: the phi joins the two predecessors just built, both carrying
        // an `i64`.
        let result = unsafe {
            let phi = LLVMBuildPhi(builder, types.i64, c"div.result".as_ptr());
            let mut values = [overflow_value.0, normal_value.0];
            let mut blocks = [overflow_value.1, normal_value.1];
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            phi
        };
        Ok(result)
    }

    /// Lowers `==`/`!=` on strings through the runtime helper.
    fn lower_string_compare(
        &mut self,
        op: IrBinOp,
        left: LLVMValueRef,
        right: LLVMValueRef,
    ) -> LLVMValueRef {
        let equal = self.call(self.codegen.runtime.str_eq, &mut [left, right], c"str.eq");
        let builder = self.codegen.builder;
        let types = self.codegen.types;
        // SAFETY: the helper returns an `i8` of 0 or 1; comparing it against
        // the appropriate constant yields the `i1` Kira booleans are.
        unsafe {
            let expected = LLVMConstInt(types.i8, u64::from(op == IrBinOp::EqStr), 0);
            LLVMBuildICmp(
                builder,
                LLVMIntPredicate::LLVMIntEQ,
                equal,
                expected,
                c"str.cmp".as_ptr(),
            )
        }
    }

    /// Lowers `&&`/`||` as branches, evaluating the right operand only when the
    /// left does not already decide the answer.
    ///
    /// `short_circuit_on` is the left value that fixes the result: `true` for
    /// `||`, `false` for `&&`.
    fn lower_short_circuit(
        &mut self,
        lhs: IrExprId,
        rhs: IrExprId,
        short_circuit_on: bool,
    ) -> Result<LLVMValueRef, LlvmError> {
        let left = self.lower_expr(lhs)?;
        let function = self.current_function();
        let rhs_block = self.append_block(function, c"logic.rhs");
        let done_block = self.append_block(function, c"logic.end");
        let builder = self.codegen.builder;

        // SAFETY: both blocks belong to the function being built and `left` is
        // an `i1`; the branch records which block reaches the join.
        let left_exit = unsafe {
            let (on_true, on_false) = if short_circuit_on {
                (done_block, rhs_block)
            } else {
                (rhs_block, done_block)
            };
            LLVMBuildCondBr(builder, left, on_true, on_false);
            LLVMGetInsertBlock(builder)
        };

        self.position_at(rhs_block);
        let right = self.lower_expr(rhs)?;
        // SAFETY: the right operand's block is unterminated; join it to the end.
        let right_exit = unsafe {
            LLVMBuildBr(builder, done_block);
            LLVMGetInsertBlock(builder)
        };

        self.position_at(done_block);
        // SAFETY: the phi joins the two predecessors just built, both `i1`.
        let result = unsafe {
            let phi = LLVMBuildPhi(builder, self.codegen.types.i1, c"logic".as_ptr());
            let short_circuited =
                LLVMConstInt(self.codegen.types.i1, u64::from(short_circuit_on), 0);
            let mut values = [short_circuited, right];
            let mut blocks = [left_exit, right_exit];
            LLVMAddIncoming(phi, values.as_mut_ptr(), blocks.as_mut_ptr(), 2);
            phi
        };
        Ok(result)
    }

    /// Lowers a call to `print` or a user function.
    fn lower_call(
        &mut self,
        callee: IrCallee,
        args: &[IrExprId],
    ) -> Result<LLVMValueRef, LlvmError> {
        match callee {
            IrCallee::Print => {
                let argument = *args
                    .first()
                    .ok_or(LlvmError::Unsupported("a print with no argument"))?;
                let ty = self.type_of(argument);
                let mut value = self.lower_expr(argument)?;
                let helper = match ty {
                    Type::Int => self.codegen.runtime.print_int,
                    Type::Float => self.codegen.runtime.print_float,
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
                    Type::Void | Type::Error => {
                        return Err(LlvmError::Unsupported("printing a value with no type"));
                    }
                };
                Ok(self.call(helper, &mut [value], c""))
            }
            IrCallee::User(index) => {
                let target = *self
                    .codegen
                    .functions
                    .get(index as usize)
                    .ok_or(LlvmError::Unsupported("a call to an unknown function"))?;
                // Arguments evaluate left to right, as the VM pushes them.
                let mut values = Vec::with_capacity(args.len());
                for &argument in args {
                    values.push(self.lower_expr(argument)?);
                }
                let returns_value =
                    self.codegen.program.functions[index as usize].return_type != Type::Void;
                let name = if returns_value { c"call" } else { c"" };
                Ok(self.call(target, &mut values, name))
            }
        }
    }

    /// Frees a string handle through the runtime.
    fn free_string(&mut self, value: LLVMValueRef) {
        self.call(self.codegen.runtime.str_free, &mut [value], c"");
    }

    /// Emits a call to `callable`.
    fn call(
        &mut self,
        callable: Callable,
        args: &mut [LLVMValueRef],
        name: &std::ffi::CStr,
    ) -> LLVMValueRef {
        // SAFETY: the builder is on a live block and `args` matches the
        // callable's signature at every call site above.
        unsafe { self.codegen.call_runtime(callable, args, name) }
    }

    /// The static type of an expression in this function's scope.
    fn type_of(&self, id: IrExprId) -> Type {
        self.codegen.program.expr_type(self.function, id)
    }

    /// The declared type of a local slot.
    fn local_type(&self, slot: u32) -> Result<Type, LlvmError> {
        self.function
            .locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::Unsupported("a read of an unknown local"))
    }

    /// The `alloca` backing a local slot.
    fn local_pointer(&self, slot: u32) -> Result<LLVMValueRef, LlvmError> {
        self.locals
            .get(slot as usize)
            .copied()
            .ok_or(LlvmError::Unsupported("a read of an unknown local"))
    }

    /// The function currently being built.
    fn current_function(&self) -> LLVMValueRef {
        // SAFETY: the builder is always positioned inside a function while a
        // body is being lowered.
        unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.codegen.builder)) }
    }

    /// Appends a fresh block to `function`.
    fn append_block(&self, function: LLVMValueRef, name: &std::ffi::CStr) -> LLVMBasicBlockRef {
        // SAFETY: `function` is a live function in this module's context.
        unsafe { LLVMAppendBasicBlockInContext(self.codegen.context, function, name.as_ptr()) }
    }

    /// Moves the builder to the end of `block`.
    fn position_at(&self, block: LLVMBasicBlockRef) {
        // SAFETY: `block` belongs to the function being built.
        unsafe { LLVMPositionBuilderAtEnd(self.codegen.builder, block) };
    }

    /// Whether the block being built already ends in a terminator.
    fn block_is_terminated(&self) -> bool {
        // SAFETY: the builder is positioned on a live block whenever this is
        // asked.
        unsafe { !LLVMGetBasicBlockTerminator(LLVMGetInsertBlock(self.codegen.builder)).is_null() }
    }
}

/// The integer predicate a comparison operator lowers to, if it is one.
fn integer_predicate(op: IrBinOp) -> Option<LLVMIntPredicate> {
    Some(match op {
        // Booleans are `i1`, so their comparisons are integer comparisons.
        IrBinOp::EqInt | IrBinOp::EqBool => LLVMIntPredicate::LLVMIntEQ,
        IrBinOp::NeInt | IrBinOp::NeBool => LLVMIntPredicate::LLVMIntNE,
        IrBinOp::LtInt => LLVMIntPredicate::LLVMIntSLT,
        IrBinOp::LeInt => LLVMIntPredicate::LLVMIntSLE,
        IrBinOp::GtInt => LLVMIntPredicate::LLVMIntSGT,
        IrBinOp::GeInt => LLVMIntPredicate::LLVMIntSGE,
        _ => return None,
    })
}

/// The float predicate a comparison operator lowers to, if it is one.
///
/// Ordered predicates match Rust's `f64` comparisons, where any comparison with
/// a NaN is false — except `!=`, which is `!(a == b)` and so is true for NaN.
fn real_predicate(op: IrBinOp) -> Option<LLVMRealPredicate> {
    Some(match op {
        IrBinOp::EqFloat => LLVMRealPredicate::LLVMRealOEQ,
        IrBinOp::NeFloat => LLVMRealPredicate::LLVMRealUNE,
        IrBinOp::LtFloat => LLVMRealPredicate::LLVMRealOLT,
        IrBinOp::LeFloat => LLVMRealPredicate::LLVMRealOLE,
        IrBinOp::GtFloat => LLVMRealPredicate::LLVMRealOGT,
        IrBinOp::GeFloat => LLVMRealPredicate::LLVMRealOGE,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_comparison_operator_has_exactly_one_predicate() {
        // A comparison must lower through one path or the other; an operator
        // that answers to both (or neither) would silently mis-lower.
        for op in [
            IrBinOp::EqInt,
            IrBinOp::NeInt,
            IrBinOp::LtInt,
            IrBinOp::LeInt,
            IrBinOp::GtInt,
            IrBinOp::GeInt,
            IrBinOp::EqBool,
            IrBinOp::NeBool,
            IrBinOp::EqFloat,
            IrBinOp::NeFloat,
            IrBinOp::LtFloat,
            IrBinOp::LeFloat,
            IrBinOp::GtFloat,
            IrBinOp::GeFloat,
        ] {
            assert_ne!(
                integer_predicate(op).is_some(),
                real_predicate(op).is_some(),
                "{op:?} must lower through exactly one comparison path",
            );
        }
    }

    #[test]
    fn arithmetic_operators_are_not_comparisons() {
        for op in [
            IrBinOp::AddInt,
            IrBinOp::DivInt,
            IrBinOp::AddFloat,
            IrBinOp::ConcatStr,
        ] {
            assert!(integer_predicate(op).is_none() && real_predicate(op).is_none());
        }
    }
}
