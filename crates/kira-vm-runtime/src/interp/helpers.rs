use super::*;

impl Vm<'_> {
    pub(super) fn report_trap_context(&self, error: &VmError) {
        if !trap_context_enabled() || !matches!(error, VmError::NotAnArray) {
            return;
        }
        let Some(probe) = &self.trap_probe else {
            return;
        };
        eprintln!(
            "kira vm trap context: function {} `{}` pc={} instruction={}",
            probe.function_id, probe.function_name, probe.pc, probe.instruction
        );
        eprintln!("  locals: [{}]", probe.locals.join(", "));
        eprintln!("  stack: [{}]", probe.stack.join(", "));
        eprintln!("  backtrace: {}", probe.backtrace.join(" <- "));
    }

    #[inline(always)]
    pub(super) fn jump(
        &self,
        module: &Module,
        frame: &mut Frame,
        target: u64,
    ) -> Result<(), VmError> {
        let Some(function) = usize::try_from(frame.func)
            .ok()
            .and_then(|index| module.functions.get(index))
        else {
            return Err(VmError::UnknownFunction(frame.func));
        };
        let len = function.code.len() as u64;
        // A target must land on a real instruction; `len` (one past the end)
        // is out of range and would read past the code on the next step.
        if target >= len {
            return Err(VmError::BadJump(target));
        }
        frame.pc = usize::try_from(target).map_err(|_| VmError::BadJump(target))?;
        Ok(())
    }

    // ----- operand-stack helpers ---------------------------------------

    /// Pops a pointer word addressing C storage.
    pub(super) fn pop_foreign_pointer(&mut self) -> Result<u64, VmError> {
        let value = self.pop()?;
        let Value::RawPtr(address) = value else {
            self.heap.drop_value(value);
            return Err(VmError::TypeMismatch {
                expected: "a pointer into C storage",
            });
        };
        Ok(address)
    }

    #[inline(always)]
    pub(super) fn pop(&mut self) -> Result<Value, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    /// Pops `count` operands, returned in pop order (the top of the stack
    /// first).
    ///
    /// On underflow every value already popped is freed before the error
    /// returns: a popped value is this VM's to own, and validation proves
    /// structure rather than stack typing, so an ill-typed module must trap
    /// without stranding storage in a heap that may outlive the call.
    pub(super) fn pop_operands(&mut self, count: usize) -> Result<Vec<Value>, VmError> {
        let mut operands = Vec::with_capacity(count);
        for _ in 0..count {
            match self.pop() {
                Ok(value) => operands.push(value),
                Err(error) => {
                    for operand in operands {
                        self.heap.drop_value(operand);
                    }
                    return Err(error);
                }
            }
        }
        Ok(operands)
    }

    /// Runs a callback with the reusable string-argument buffer removed from
    /// the VM, then returns the empty buffer to the VM for the next operation.
    pub(super) fn with_string_args<R>(
        &mut self,
        body: impl FnOnce(&mut Self, &mut Vec<Value>) -> R,
    ) -> R {
        let mut arguments = std::mem::take(&mut self.string_args);
        let result = body(self, &mut arguments);
        arguments.clear();
        self.string_args = arguments;
        result
    }

    /// Reports a mismatched operand, freeing it first.
    ///
    /// The typed pops below take the value off the stack before they know it is
    /// the wrong one, and a popped value is this VM's to own. Well-typed
    /// bytecode never reaches here, but a `Module` is a public artifact and
    /// validation proves structure rather than stack typing — so an ill-typed
    /// module must trap without stranding storage in a heap that may outlive
    /// the call.
    pub(super) fn mismatch(&mut self, value: Value, expected: &'static str) -> VmError {
        self.heap.drop_value(value);
        VmError::TypeMismatch { expected }
    }

    #[inline(always)]
    pub(super) fn pop_int(&mut self) -> Result<i64, VmError> {
        match self.pop()? {
            Value::Int(value) => Ok(value),
            other => Err(self.mismatch(other, "Int")),
        }
    }

    #[inline(always)]
    pub(super) fn pop_float(&mut self) -> Result<f64, VmError> {
        match self.pop()? {
            Value::Float(value) => Ok(value),
            other => Err(self.mismatch(other, "Float")),
        }
    }

    #[inline(always)]
    pub(super) fn pop_bool(&mut self) -> Result<bool, VmError> {
        match self.pop()? {
            Value::Bool(value) => Ok(value),
            other => Err(self.mismatch(other, "Bool")),
        }
    }

    #[inline(always)]
    pub(super) fn pop_str(&mut self) -> Result<crate::value::StrId, VmError> {
        match self.pop()? {
            Value::Str(id) => Ok(id),
            other => Err(self.mismatch(other, "String")),
        }
    }

    /// Pops the two string operands of a binary string op, right one first.
    ///
    /// Paired here rather than at each call site because the second pop is the
    /// one that can fail with the first already in hand: an ill-typed module
    /// would otherwise strand the right operand in a local no unwind can see.
    pub(super) fn pop_two_str(
        &mut self,
    ) -> Result<(crate::value::StrId, crate::value::StrId), VmError> {
        let rhs = self.pop_str()?;
        match self.pop_str() {
            Ok(lhs) => Ok((lhs, rhs)),
            Err(error) => {
                self.heap.free(rhs);
                Err(error)
            }
        }
    }
}
