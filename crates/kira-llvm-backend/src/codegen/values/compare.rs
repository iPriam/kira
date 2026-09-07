//! Equality walks and drop-body invocation for LLVM values.

use kira_semantics_model::Type;
use llvm_sys::core::*;
use llvm_sys::prelude::*;

use super::super::Codegen;

impl Codegen<'_> {
    /// Whether two values of `ty` are structurally equal, as an `i1`.
    ///
    /// Mirrors the VM's `Heap::values_equal`, and is reached the same way: only
    /// from an erasure, where both sides are already known to be the same Kira
    /// type. That is what lets this walk a struct field-by-field without
    /// checking anything about the operands first — the erasure box's tag
    /// settled it.
    ///
    /// Neither operand is consumed. A comparison reads and takes nothing, so a
    /// caller still owns both afterwards.
    ///
    /// By pointer for the same reason [`Codegen::retain_at_walk`] is: a struct's
    /// field is compared where it lies rather than by loading the struct around
    /// it twice.
    ///
    /// The walk, emitted into a type's equality leaf. A struct field goes back
    /// through [`Codegen::equal_at`], which is where the recursion becomes a
    /// call — see [`super::glue`].
    pub(in crate::codegen) fn equal_at_walk(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        let builder = self.builder;
        match ty {
            // A float compares as IEEE says, so `NaN` equals nothing: the same
            // rule `EqFloat` follows, and the VM's arm alongside it.
            Type::Float(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands are `double` and the builder is live.
                Ok(unsafe {
                    LLVMBuildFCmp(
                        builder,
                        llvm_sys::LLVMRealPredicate::LLVMRealOEQ,
                        a,
                        b,
                        c"eq.float".as_ptr(),
                    )
                })
            }
            Type::Int(_) | Type::Bool | Type::RawPtr | Type::ForeignPtr(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands share one integer type and the builder
                // is live.
                Ok(unsafe {
                    LLVMBuildICmp(
                        builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        a,
                        b,
                        c"eq.scalar".as_ptr(),
                    )
                })
            }
            // A cell has reference semantics, so identity *is* its equality:
            // two boxes holding equal values are still two places to write.
            // The same rule the VM applies (`Heap::objects_equal`), and it has
            // to be the same one — a captured `var` inside a struct reaches
            // here whenever that struct is erased, because erasing an aggregate
            // emits the equality leaf that walks it.
            Type::Cell(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: a cell is one opaque pointer on both sides and the
                // builder is live; `icmp eq` on two pointers is their identity.
                Ok(unsafe {
                    LLVMBuildICmp(
                        builder,
                        llvm_sys::LLVMIntPredicate::LLVMIntEQ,
                        a,
                        b,
                        c"eq.cell".as_ptr(),
                    )
                })
            }
            // The helper consumes what it compares, so each side is cloned for
            // it: the values themselves belong to whoever called this.
            Type::String => {
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(right, ty)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.str_eq, &mut [a, b], c"eq.str");
                Ok(self.truthy(equal))
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let field_types = self.field_types(id)?;
                // An empty struct is a value with nothing to disagree about.
                let mut all = self.const_bool(true);
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    let index = index as u32;
                    let (a, b) = (
                        self.field_pointer(struct_type, left, index),
                        self.field_pointer(struct_type, right, index),
                    );
                    let equal = self.equal_at(a, b, field_ty)?;
                    // SAFETY: both are `i1` and the builder is live. `and`
                    // rather than a branch chain: a field comparison has no
                    // side effect to skip, so there is nothing to short-circuit
                    // for beyond the work itself.
                    all = unsafe { LLVMBuildAnd(builder, all, equal, c"eq.field".as_ptr()) };
                }
                Ok(all)
            }
            // Both reach the runtime, which walks the elements or the tag and
            // payload. An array needs its element's leaf to compare items it
            // cannot type; an enum box carries everything its comparison needs.
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let eq = self.element_eq(element)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.array_eq, &mut [a, b, esize, eq], c"eq.array");
                Ok(self.truthy(equal))
            }
            Type::Enum(_) | Type::Any => {
                let (a, b) = self.load_operands(left, right, ty)?;
                let equal = self.call(self.runtime.any_eq, &mut [a, b], c"eq.enum");
                Ok(self.truthy(equal))
            }
            // Nothing else can be inside an erased value: `Void`, `Error`,
            // `CString`, a cell, a task, and callback state are all refused by
            // `Type::assignable_to` before `Any` takes them, and none is a
            // struct field type that could carry one in sideways.
            other => Err(crate::LlvmError::internal(format!(
                "an equality of `{other:?}`, which no erasure admits,"
            ))),
        }
    }

    /// Reads both sides of a comparison out of the storage holding them.
    pub(in crate::codegen) fn load_operands(
        &self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<(LLVMValueRef, LLVMValueRef), crate::LlvmError> {
        let llvm_type = self.llvm_type(ty)?;
        // SAFETY: both address a live value of `llvm_type` and the builder is
        // on a live block.
        Ok(unsafe {
            (
                LLVMBuildLoad2(self.builder, llvm_type, left, c"eq.a".as_ptr()),
                LLVMBuildLoad2(self.builder, llvm_type, right, c"eq.b".as_ptr()),
            )
        })
    }

    /// Calls a type's user `Drop` body on the value at `at`.
    ///
    /// The body takes its receiver the way every method does — by pointer when
    /// this module lends, by value otherwise — so the address is loaded only
    /// where the signature asks for a value. Either way the body owns nothing:
    /// the members are released by the walk that follows this call, which is
    /// why the glue's own release plan excludes its receiver.
    pub(in crate::codegen) fn call_drop_glue(
        &mut self,
        at: LLVMValueRef,
        glue: u32,
    ) -> Result<(), crate::LlvmError> {
        let callee =
            self.program.functions.get(glue as usize).ok_or_else(|| {
                crate::LlvmError::internal("a `Drop` body the module never declared")
            })?;
        let by_pointer = self.param_is_pointer(callee, 0);
        let receiver = callee.locals.first().copied().unwrap_or(Type::Void);
        let target = self.functions.get(glue as usize).copied().flatten().ok_or(
            crate::LlvmError::internal("a `Drop` body compiled for the other engine"),
        )?;
        let argument = match by_pointer {
            true => at,
            false => {
                let llvm_type = self.llvm_type(receiver)?;
                // SAFETY: `at` addresses a live value of the receiver's type and
                // the builder is on a live block.
                unsafe { LLVMBuildLoad2(self.builder, llvm_type, at, c"drop.self".as_ptr()) }
            }
        };
        self.call(target, &mut [argument], c"");
        Ok(())
    }
}
