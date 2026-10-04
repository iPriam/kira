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
            Type::Int(_)
            | Type::Bool
            | Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::NativeState(_) => {
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
            // A `Number` compares through the same helper `Number == Number`
            // uses, and that helper frees the two handles it takes, so each side
            // is retained for it exactly as a string is.
            Type::Number => {
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(right, ty)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let number_equal = self.runtime.number_ops
                    [usize::from(kira_runtime_abi::NumberOp::Equal.as_byte())];
                // The number helpers already answer the `i1` Kira booleans are,
                // unlike the `i8`-returning string and array helpers, so this is
                // the comparison result directly.
                Ok(self.call(number_equal, &mut [a, b], c"eq.number"))
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
            // A distinct type is one scalar word laid out exactly as its
            // representation, so it compares as that word: the same storage,
            // read through the representation's own leaf. This is the one field
            // shape whose written type differs from the type its bytes compare
            // as, which is why it recurses rather than matching a scalar arm.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.equal_at_walk(left, right, representation)
            }
            // Nothing else can be inside an erased value. NativeState itself
            // is still refused as an erased root, but an affine aggregate may
            // carry one as a field; its structural equality compares the opaque
            // state token by identity above, exactly as the VM does.
            other => Err(crate::LlvmError::internal(format!(
                "an equality of `{other:?}`, which no erasure admits,"
            ))),
        }
    }

    /// The three-way order of two values of `ty`, as an `i8` sign — negative,
    /// zero, or positive — the ordering twin of [`Codegen::equal_at_walk`].
    ///
    /// Mirrors the VM's `Heap::compare_values` leaf for leaf, so the two engines
    /// never disagree: a signed integer and a boolean by their value, a float by
    /// the ordered comparisons (so a `NaN` falls into the equal bucket, as the VM
    /// does), a string by its bytes, a `Number` by its decimal value, a struct
    /// field by field, an array element by element then by length. Only a type
    /// the frontend proved `Ordered` reaches here, and unsigned integers are
    /// refused there (their width is erased by the time the walk sees them), so
    /// every leaf below is one this can order with the right rule.
    ///
    /// An enum is refused rather than lowered: its native ordering would need a
    /// tag-order helper and a cmp leaf threaded through the erasure box, which is
    /// not built yet, so a program ordering one names that at the build rather
    /// than diverging from the VM.
    ///
    /// Neither operand is consumed, exactly as the equality walk takes nothing.
    pub(in crate::codegen) fn compare_at_walk(
        &mut self,
        left: LLVMValueRef,
        right: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        let builder = self.builder;
        match ty {
            // A float orders by the *ordered* comparisons, so a `NaN` is neither
            // less nor greater and lands at `0` — the same bucket the VM's
            // `a < b`/`a > b` chain gives it.
            Type::Float(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands are `double` and the builder is live.
                let (lt, gt) = unsafe {
                    (
                        LLVMBuildFCmp(
                            builder,
                            llvm_sys::LLVMRealPredicate::LLVMRealOLT,
                            a,
                            b,
                            c"cmp.flt.lt".as_ptr(),
                        ),
                        LLVMBuildFCmp(
                            builder,
                            llvm_sys::LLVMRealPredicate::LLVMRealOGT,
                            a,
                            b,
                            c"cmp.flt.gt".as_ptr(),
                        ),
                    )
                };
                Ok(self.sign_from(lt, gt))
            }
            // A signed integer and a boolean order by a signed compare: the VM
            // holds every integer as one `i64` and compares it signed, and a
            // width narrower than `i64` was sign-extended into it, so the signed
            // predicate at the field's own width agrees. Unsigned widths never
            // reach here.
            Type::Int(_) | Type::Bool => {
                let (a, b) = self.load_operands(left, right, ty)?;
                // SAFETY: both operands share one integer type and the builder
                // is live.
                let (lt, gt) = unsafe {
                    (
                        LLVMBuildICmp(
                            builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSLT,
                            a,
                            b,
                            c"cmp.int.lt".as_ptr(),
                        ),
                        LLVMBuildICmp(
                            builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntSGT,
                            a,
                            b,
                            c"cmp.int.gt".as_ptr(),
                        ),
                    )
                };
                Ok(self.sign_from(lt, gt))
            }
            // The helper consumes what it compares, so each side is cloned for
            // it, exactly as the string equality arm does.
            Type::String => {
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(right, ty)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                Ok(self.call(self.runtime.str_cmp, &mut [a, b], c"cmp.str"))
            }
            // A `Number` has no single three-way helper, so its order is built
            // from the two the ABI has: `Equal` decides the zero, and `Less`
            // decides the sign of the rest. Each consumes its two handles, so
            // each side is retained once per call — twice in all — the same
            // cloning the equality arm does once.
            Type::Number => {
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(left, ty)?;
                self.retain_at_walk(right, ty)?;
                self.retain_at_walk(right, ty)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                let less_op = self.runtime.number_ops
                    [usize::from(kira_runtime_abi::NumberOp::Less.as_byte())];
                let equal_op = self.runtime.number_ops
                    [usize::from(kira_runtime_abi::NumberOp::Equal.as_byte())];
                // Both helpers answer the `i1` Kira booleans are.
                let less = self.call(less_op, &mut [a, b], c"cmp.num.lt");
                let equal = self.call(equal_op, &mut [a, b], c"cmp.num.eq");
                // sign = equal ? 0 : (less ? -1 : 1)
                // SAFETY: `less`/`equal` are `i1` and the builder is live.
                Ok(unsafe {
                    let neg_one = LLVMConstInt(self.types.i8, (-1i64) as u64, 1);
                    let one = LLVMConstInt(self.types.i8, 1, 0);
                    let zero = LLVMConstInt(self.types.i8, 0, 0);
                    let non_zero =
                        LLVMBuildSelect(builder, less, neg_one, one, c"cmp.num.sign".as_ptr());
                    LLVMBuildSelect(builder, equal, zero, non_zero, c"cmp.num".as_ptr())
                })
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let field_types = self.field_types(id)?;
                // An empty struct is a value with nothing to disagree about.
                let mut sign = unsafe { LLVMConstInt(self.types.i8, 0, 0) };
                let zero = unsafe { LLVMConstInt(self.types.i8, 0, 0) };
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    let index = index as u32;
                    let (a, b) = (
                        self.field_pointer(struct_type, left, index),
                        self.field_pointer(struct_type, right, index),
                    );
                    let field_sign = self.compare_at(a, b, field_ty)?;
                    // Keep the running sign once it is non-zero — the first
                    // differing field decides. A field comparison has no side
                    // effect, so folding with a select rather than branching is
                    // the same choice the equality walk makes with `and`.
                    // SAFETY: all are `i8`/`i1` and the builder is live.
                    sign = unsafe {
                        let decided = LLVMBuildICmp(
                            builder,
                            llvm_sys::LLVMIntPredicate::LLVMIntNE,
                            sign,
                            zero,
                            c"cmp.decided".as_ptr(),
                        );
                        LLVMBuildSelect(builder, decided, sign, field_sign, c"cmp.field".as_ptr())
                    };
                }
                Ok(sign)
            }
            // The runtime walks the elements, comparing each with the element's
            // own cmp leaf and breaking a tie by length — the same lexicographic
            // rule the VM's array arm follows.
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let cmp = self.element_cmp(element)?;
                let (a, b) = self.load_operands(left, right, ty)?;
                Ok(self.call(
                    self.runtime.array_cmp,
                    &mut [a, b, esize, cmp],
                    c"cmp.array",
                ))
            }
            // A distinct type orders as the one scalar word it is, read through
            // its representation, exactly as the equality walk does.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.compare_at_walk(left, right, representation)
            }
            // An enum orders through the runtime, which walks its tag and then
            // its payload — a scalar, float, string, nested enum, or an aggregate
            // through the cmp leaf its box carries — byte-for-byte the VM's
            // `compare_values`. Reading takes nothing, so neither operand is
            // retained.
            Type::Enum(_) => {
                let (a, b) = self.load_operands(left, right, ty)?;
                Ok(self.call(self.runtime.any_cmp, &mut [a, b], c"cmp.enum"))
            }
            // `Any` has no total order the frontend would admit here.
            Type::Any => Err(crate::LlvmError::unsupported(
                "structural ordering of an erased `Any` value",
            )),
            other => Err(crate::LlvmError::internal(format!(
                "an ordering of `{other:?}`, which is not `Ordered`,"
            ))),
        }
    }

    /// The `i8` sign `-1 / 0 / 1` from the two ordered comparisons of one leaf.
    ///
    /// `less` and `greater` are the `i1` results of "a before b" and "a after
    /// b"; both false is the equal (and, for floats, the unordered) case, which
    /// is `0`. One folder for every scalar arm so they cannot drift.
    fn sign_from(&self, less: LLVMValueRef, greater: LLVMValueRef) -> LLVMValueRef {
        // SAFETY: both inputs are `i1` and the builder is on a live block.
        unsafe {
            let neg_one = LLVMConstInt(self.types.i8, (-1i64) as u64, 1);
            let one = LLVMConstInt(self.types.i8, 1, 0);
            let zero = LLVMConstInt(self.types.i8, 0, 0);
            let non_neg = LLVMBuildSelect(self.builder, greater, one, zero, c"cmp.gt".as_ptr());
            LLVMBuildSelect(self.builder, less, neg_one, non_neg, c"cmp.sign".as_ptr())
        }
    }

    /// Folds the value at `at` into the running hash accumulator `acc`, returning
    /// the updated accumulator (`i64`) — what `HashValue` walks.
    ///
    /// The fold twin of [`Codegen::equal_at_walk`], matching the VM's `hash_into`
    /// byte for byte so the two engines answer the same `hash(v)`: an integer
    /// folds its eight little-endian bytes (every width widened to the `i64` the
    /// VM holds), a boolean its one byte, a string its bytes, a struct its fields
    /// in order, an array its length then elements, a payload-less enum its tag.
    /// All the FNV math is in the runtime (Rust), so only the byte gathering and
    /// the walk are emitted here. A float never reaches it — the classifier
    /// refused it — and a payload-carrying enum is refused, as ordering's is,
    /// until its box walk exists.
    ///
    /// Neither the value nor its parts are consumed.
    pub(in crate::codegen) fn hash_at_walk(
        &mut self,
        acc: LLVMValueRef,
        at: LLVMValueRef,
        ty: Type,
    ) -> Result<LLVMValueRef, crate::LlvmError> {
        let builder = self.builder;
        match ty {
            // A boolean folds its one byte, matching the VM's single-byte feed.
            Type::Bool => {
                let one = self.const_i64(1);
                Ok(self.call(self.runtime.hash_bytes, &mut [acc, at, one], c"hash.bool"))
            }
            // An integer folds the eight bytes of the `i64` the VM holds: a
            // narrower width is widened first (signed widths sign-extend, unsigned
            // zero-extend) so the bytes match what the VM fed.
            Type::Int(_) => {
                let (value, _) = self.load_operands(at, at, ty)?;
                let widened = self.widen_to_i64(value, ty);
                let slot = self.hash_scratch(self.types.i64);
                // SAFETY: `slot` holds an `i64` and the builder is live.
                unsafe { LLVMBuildStore(builder, widened, slot) };
                let eight = self.const_i64(8);
                Ok(self.call(
                    self.runtime.hash_bytes,
                    &mut [acc, slot, eight],
                    c"hash.int",
                ))
            }
            // The string's bytes fold through the runtime, which borrows the
            // handle and takes nothing.
            Type::String => {
                let (handle, _) = self.load_operands(at, at, ty)?;
                Ok(self.call(self.runtime.hash_str, &mut [acc, handle], c"hash.str"))
            }
            Type::Struct(id) => {
                let struct_type = self.llvm_type(ty)?;
                let field_types = self.field_types(id)?;
                let mut acc = acc;
                for (index, field_ty) in field_types.into_iter().enumerate() {
                    let field = self.field_pointer(struct_type, at, index as u32);
                    acc = self.hash_at(acc, field, field_ty)?;
                }
                Ok(acc)
            }
            Type::Array(_) => {
                let element = self.element_of(ty)?;
                let esize = self.abi_size(element)?;
                let leaf = self.element_hash(element)?;
                let (handle, _) = self.load_operands(at, at, ty)?;
                Ok(self.call(
                    self.runtime.hash_array,
                    &mut [acc, handle, esize, leaf],
                    c"hash.array",
                ))
            }
            // An enum folds through the runtime, which walks its tag and then its
            // payload — a scalar, string, nested enum, or an aggregate through the
            // hash leaf its box carries — byte-for-byte the VM's `hash_into`.
            // Reading takes nothing.
            Type::Enum(_) => {
                let (handle, _) = self.load_operands(at, at, ty)?;
                Ok(self.call(self.runtime.any_hash, &mut [acc, handle], c"hash.enum"))
            }
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.hash_at_walk(acc, at, representation)
            }
            other => Err(crate::LlvmError::internal(format!(
                "a hash of `{other:?}`, which is not `Hashable`,"
            ))),
        }
    }

    /// Whether a value of `ty` carries a total order this backend can compare —
    /// the same rule the frontend's `Ordered` classifier applies.
    ///
    /// Used to decide whether an aggregate enum payload gets a `cmp` leaf on its
    /// box: it does exactly when its type is `Ordered`, so a payload that is not
    /// gets a null leaf that no comparison reaches (the frontend refuses ordering
    /// such an enum). Bounded by `seen`, since a shape cannot be the reason it is
    /// itself unorderable.
    pub(in crate::codegen) fn type_orders(&self, ty: Type, seen: &mut Vec<Type>) -> bool {
        if seen.contains(&ty) {
            return true;
        }
        match ty {
            // Signed integers, floats, booleans, and strings carry a total order;
            // an unsigned width does not order structurally (its sign is erased).
            Type::Int(_) => !ty.is_unsigned_int(),
            Type::Float(_) | Type::Bool | Type::String => true,
            Type::Distinct(_) => {
                seen.push(ty);
                self.type_orders(self.program.types.representation(ty), seen)
            }
            Type::Struct(id) => {
                seen.push(ty);
                self.program.types.structs().get(id).is_some_and(|def| {
                    def.fields
                        .iter()
                        .all(|field| self.type_orders(field.ty, seen))
                })
            }
            Type::Enum(id) => {
                seen.push(ty);
                self.program.types.enums().get(id).is_some_and(|def| {
                    def.variants
                        .iter()
                        .all(|variant| variant.payload.is_none_or(|p| self.type_orders(p, seen)))
                })
            }
            Type::Array(_) => {
                seen.push(ty);
                self.program
                    .types
                    .element_of(ty)
                    .is_some_and(|element| self.type_orders(element, seen))
            }
            _ => false,
        }
    }

    /// Whether a value of `ty` folds into a hash consistent with `==` — the same
    /// rule the frontend's `Hashable` classifier applies (a float or decimal is
    /// refused, every other scalar and the aggregates of them admitted). Decides
    /// whether an aggregate enum payload gets a `hash` leaf on its box.
    pub(in crate::codegen) fn type_hashes(&self, ty: Type, seen: &mut Vec<Type>) -> bool {
        if seen.contains(&ty) {
            return true;
        }
        match ty {
            Type::Int(_) | Type::Bool | Type::String => true,
            Type::Float(_) | Type::Number => false,
            Type::Distinct(_) => {
                seen.push(ty);
                self.type_hashes(self.program.types.representation(ty), seen)
            }
            Type::Struct(id) => {
                seen.push(ty);
                self.program.types.structs().get(id).is_some_and(|def| {
                    def.fields
                        .iter()
                        .all(|field| self.type_hashes(field.ty, seen))
                })
            }
            Type::Enum(id) => {
                seen.push(ty);
                self.program.types.enums().get(id).is_some_and(|def| {
                    def.variants
                        .iter()
                        .all(|variant| variant.payload.is_none_or(|p| self.type_hashes(p, seen)))
                })
            }
            Type::Array(_) => {
                seen.push(ty);
                self.program
                    .types
                    .element_of(ty)
                    .is_some_and(|element| self.type_hashes(element, seen))
            }
            _ => false,
        }
    }

    /// Widens an integer value to the `i64` the VM folds, extending by the type's
    /// signedness so the bytes match.
    fn widen_to_i64(&self, value: LLVMValueRef, ty: Type) -> LLVMValueRef {
        // SAFETY: `value` is an integer of `ty`'s width and the builder is live.
        unsafe {
            let width = LLVMGetIntTypeWidth(LLVMTypeOf(value));
            if width >= 64 {
                return value;
            }
            if ty.is_unsigned_int() {
                LLVMBuildZExt(self.builder, value, self.types.i64, c"hash.zext".as_ptr())
            } else {
                LLVMBuildSExt(self.builder, value, self.types.i64, c"hash.sext".as_ptr())
            }
        }
    }

    /// An `i64` constant in this module's context.
    fn const_i64(&self, value: i64) -> LLVMValueRef {
        // SAFETY: `types.i64` belongs to this context; a constant needs no
        // builder position.
        unsafe { LLVMConstInt(self.types.i64, value as u64, 0) }
    }

    /// A one-per-function `i64` scratch slot the hash walk spills a scalar into
    /// so the runtime can fold its bytes.
    fn hash_scratch(&mut self, ty: LLVMTypeRef) -> LLVMValueRef {
        // SAFETY: a hash is only lowered inside a function body, so the builder
        // is positioned in one to allocate against.
        let function = unsafe { LLVMGetBasicBlockParent(LLVMGetInsertBlock(self.builder)) };
        self.entry_alloca(function, ty, c"hash.scratch")
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
