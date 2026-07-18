//! Lowering: Kira IR to WebAssembly.
//!
//! The same [`IrProgram`] the VM's bytecode compiler and the LLVM backend
//! consume. Where wasm's own semantics differ from Kira's, Kira wins and the
//! difference is paid for explicitly:
//!
//! - integer arithmetic wraps, which wasm already does — and `i64.div_s` does
//!   *not*: it traps on `Int` minimum divided by `-1` where the VM wraps, so
//!   that case is branched around rather than left to the engine,
//! - division and remainder by zero are Kira traps with Kira's message, not
//!   wasm traps with the engine's,
//! - `&&` and `||` evaluate their right operand only when the left decides
//!   nothing, matching the bytecode compiler's jumps.

use kira_ir::{IrCallee, IrExpr, IrExprId, IrFunction, IrProgram, IrStmt, IrUnOp};
use kira_semantics_model::Type;

use crate::arrays::ArrayCopies;
use crate::encode::ValType;
use crate::error::WasmError;
use crate::func::{BlockType::Empty, Func, FuncIdx, LocalIdx};
use crate::literals::Literals;
use crate::rt::Runtime;
use crate::structs::{Structs, load_field, store_field};

mod operators;
mod places;

/// Lowers one function at a time against a fixed set of handles.
pub struct Lowering<'a> {
    program: &'a IrProgram,
    runtime: &'a Runtime,
    literals: &'a mut Literals,
    functions: &'a [FuncIdx],
    structs: &'a Structs,
    /// The deep-copy helper for each array type the program mentions.
    arrays: &'a ArrayCopies,
    /// Scratch locals holding the object being built, one per level of nested
    /// struct construction.
    ///
    /// A store needs its destination address underneath the value, so building
    /// an object means keeping its address somewhere while the fields are
    /// evaluated — and a field's expression may itself build an object. One
    /// local per depth is what keeps an inner construction from clobbering the
    /// outer one's address. They are declared up front, from the depth the body
    /// actually reaches, because wasm declares all of a function's locals
    /// before its first instruction.
    scratch: Vec<LocalIdx>,
    /// How many struct constructions are open right now, which is the scratch
    /// local the next one takes.
    depth: usize,
    /// How many wasm labels enclose the instruction being emitted.
    ///
    /// wasm names a branch target by how many labels to pop, not by identity,
    /// so the same jump is a different immediate depending on where it sits.
    /// Tracking the count is what lets a `break` nested inside an `if` still
    /// find its loop.
    ///
    /// Only statement-level labels are counted. An expression's labels
    /// (short-circuit, checked division) open and close within that
    /// expression, and no statement — so no `break`/`continue` — can appear
    /// inside one.
    labels: u32,
    /// The loops enclosing the statement being lowered, innermost last.
    loops: Vec<WasmLoop>,
}

/// The two label positions a `break`/`continue` inside one loop branches to.
///
/// Each field is a label *index* counted from the function's outermost label,
/// which is stable no matter how deeply the jump itself is nested; the branch
/// immediate is derived from it at the jump site.
struct WasmLoop {
    /// The `block` wrapping the loop — the target of a `break`.
    block: u32,
    /// The `loop` itself — the target of a `continue`, which re-tests the
    /// condition.
    loop_: u32,
}

impl<'a> Lowering<'a> {
    /// Creates a lowering over `program`, whose functions were declared in
    /// order into `functions`.
    pub fn new(
        program: &'a IrProgram,
        runtime: &'a Runtime,
        literals: &'a mut Literals,
        functions: &'a [FuncIdx],
        structs: &'a Structs,
        arrays: &'a ArrayCopies,
    ) -> Self {
        Self {
            program,
            runtime,
            literals,
            functions,
            structs,
            arrays,
            scratch: Vec::new(),
            depth: 0,
            labels: 0,
            loops: Vec::new(),
        }
    }

    /// The `br` immediate that reaches the label at index `target`.
    ///
    /// `br` counts outward from the innermost enclosing label, so the
    /// immediate for a fixed target shrinks as the jump site gets shallower and
    /// grows as it nests. With `labels` enclosing labels, the innermost sits at
    /// index `labels - 1`, which is the subtraction below.
    fn branch_to(&self, target: u32) -> Result<u32, WasmError> {
        self.labels
            .checked_sub(1)
            .and_then(|innermost| innermost.checked_sub(target))
            .ok_or(WasmError::JumpOutsideLoop)
    }

    /// The wasm value type a Kira type occupies, or `None` for `Void`.
    ///
    /// `Void` has no representation: a `Void` expression leaves nothing on the
    /// stack, which is why nothing has to be dropped after a call to a function
    /// that returns nothing.
    pub fn val_type(ty: Type) -> Result<Option<ValType>, WasmError> {
        Ok(match ty {
            Type::Int(_) => Some(ValType::I64),
            Type::Float(_) => Some(ValType::F64),
            Type::Bool => Some(ValType::I32),
            // All four are addresses; `value_of` widens them to the memory's
            // width. An array's value is its header's address, and an enum's is
            // its box's.
            Type::String | Type::Struct(_) | Type::Array(_) | Type::Enum(_) => Some(ValType::I32),
            Type::Void => None,
            Type::Error => return Err(WasmError::ErrorType),
        })
    }

    /// The wasm value type a Kira type occupies, resolving `String` against the
    /// module's address width.
    fn value_of(&self, ty: Type, addr: ValType) -> Result<Option<ValType>, WasmError> {
        Ok(match Self::val_type(ty)? {
            // A `String`, a struct, an array, and an enum are addresses, so all
            // are as wide as the memory is.
            Some(ValType::I32)
                if matches!(
                    ty,
                    Type::String | Type::Struct(_) | Type::Array(_) | Type::Enum(_)
                ) =>
            {
                Some(addr)
            }
            other => other,
        })
    }

    /// Lowers `function`'s body into `func`.
    pub fn function(&mut self, func: &mut Func, function: &IrFunction) -> Result<(), WasmError> {
        let addr = func.addr().val();

        // The IR lists parameters first in the same order wasm does, so the
        // parameter slots are already the right locals; only the rest are
        // declared.
        for ty in function.locals.iter().skip(function.param_count as usize) {
            let value = self
                .value_of(*ty, addr)?
                .ok_or(WasmError::VoidLocal(function.name.clone()))?;
            func.local(value);
        }

        // Scratch locals come after the declared ones, so a slot index still
        // means the same local it does in the IR.
        let depth = self.construction_depth(&function.body);
        self.scratch = (0..depth).map(|_| func.local(addr)).collect();

        self.body(func, function, &function.body)?;

        // A Kira function always returns, so an implicit trailing return only
        // matters to wasm's validator, which requires the body to leave the
        // result on the stack. A value-returning function whose IR ends without
        // a `Return` is unreachable code, not a value to invent.
        if !func.results().is_empty() {
            func.unreachable();
        }
        Ok(())
    }

    /// Builds a struct object and leaves its address on the stack.
    ///
    /// Allocate, then fill: a store wants its destination underneath its value,
    /// so the address is parked in a scratch local while each field is
    /// evaluated. The scratch is indexed by construction depth, so a field that
    /// builds its own object takes the next one down and cannot clobber this.
    fn struct_new(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        struct_id: kira_semantics_model::StructId,
        fields: &[IrExprId],
    ) -> Result<(), WasmError> {
        let layout = self.structs.layout(struct_id)?.clone();
        let depth = self.depth;
        let object = *self.scratch.get(depth).ok_or(WasmError::Wiring)?;
        self.depth += 1;

        func.i32_const(layout.size as i32).i32_to_addr();
        func.call(self.runtime.alloc);
        func.local_set(object);

        let addr = func.addr();
        for (index, &field) in fields.iter().enumerate() {
            let offset = u64::from(*layout.offsets.get(index).ok_or(WasmError::UnknownStruct)?);
            let ty = self.program.expr_type(function, field);
            func.local_get(object);
            self.expr(func, function, field)?;
            store_field(func, ty, addr, offset)?;
        }

        func.local_get(object);
        self.depth -= 1;
        Ok(())
    }

    /// Builds an enum box and leaves its address on the stack.
    ///
    /// The box is a fixed 16 bytes: the discriminant as an `i64` at offset 0 —
    /// wide on both memories so a tag read is one `i64.load` — and the optional
    /// payload at offset 8. Allocate, then fill, the same shape a struct build
    /// uses: the address is parked in a scratch local while the payload is
    /// evaluated. The heap never frees, so nothing here has to.
    fn enum_new(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        enum_id: kira_semantics_model::EnumId,
        tag: u32,
        payload: Option<IrExprId>,
    ) -> Result<(), WasmError> {
        let box_size: i32 = 16;
        let depth = self.depth;
        let object = *self.scratch.get(depth).ok_or(WasmError::Wiring)?;
        self.depth += 1;

        func.i32_const(box_size).i32_to_addr();
        func.call(self.runtime.alloc);
        func.local_set(object);

        // The discriminant, as an `i64` at offset 0.
        func.local_get(object);
        func.i64_const(i64::from(tag));
        func.i64_store(0);

        if let Some(payload) = payload {
            let payload_ty = self
                .program
                .types
                .enums()
                .get(enum_id)
                .and_then(|def| def.variant(tag))
                .and_then(|variant| variant.payload)
                .ok_or(WasmError::Wiring)?;
            let addr = func.addr();
            func.local_get(object);
            self.expr(func, function, payload)?;
            store_field(func, payload_ty, addr, 8)?;
        }

        func.local_get(object);
        self.depth -= 1;
        Ok(())
    }

    /// Lowers a statement list.
    fn body(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        body: &[IrStmt],
    ) -> Result<(), WasmError> {
        for statement in body {
            self.statement(func, function, statement)?;
        }
        Ok(())
    }

    /// Lowers one statement.
    fn statement(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        statement: &IrStmt,
    ) -> Result<(), WasmError> {
        match statement {
            IrStmt::Let { local, init } => {
                self.expr(func, function, *init)?;
                func.local_set(LocalIdx(*local));
            }
            IrStmt::Assign { place, value } => self.store_place(func, function, place, *value)?,
            IrStmt::Return { value } => {
                if let Some(value) = value {
                    self.expr(func, function, *value)?;
                }
                func.return_();
            }
            IrStmt::Eval { expr } => {
                self.expr(func, function, *expr)?;
                // A value evaluated for effect is dropped; a `Void` one left
                // nothing to drop.
                let ty = self.program.expr_type(function, *expr);
                if Self::val_type(ty)?.is_some() {
                    func.drop();
                }
            }
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => {
                self.expr(func, function, *cond)?;
                func.if_(Empty);
                self.labels += 1;
                self.body(func, function, then_body)?;
                if !else_body.is_empty() {
                    func.else_();
                    self.body(func, function, else_body)?;
                }
                self.labels -= 1;
                func.end();
            }
            IrStmt::While { cond, body } => {
                // block { loop { br_if 1 (!cond); body; br 0 } }: the condition
                // is tested before each iteration, including the first.
                let block = self.labels;
                func.block(Empty);
                self.labels += 1;
                let loop_ = self.labels;
                func.loop_(Empty);
                self.labels += 1;

                self.expr(func, function, *cond)?;
                let to_end = self.branch_to(block)?;
                func.i32_eqz().br_if(to_end);

                self.loops.push(WasmLoop { block, loop_ });
                let lowered = self.body(func, function, body);
                self.loops.pop();
                lowered?;

                let to_start = self.branch_to(loop_)?;
                func.br(to_start);
                self.labels -= 2;
                func.end();
                func.end();
            }
            IrStmt::Break => {
                let block = self.innermost_loop()?.block;
                let target = self.branch_to(block)?;
                func.br(target);
            }
            IrStmt::Continue => {
                let loop_ = self.innermost_loop()?.loop_;
                let target = self.branch_to(loop_)?;
                func.br(target);
            }
        }
        Ok(())
    }

    /// The innermost enclosing loop's label positions.
    ///
    /// Analysis rejects a `break`/`continue` outside a loop, so an empty stack
    /// here means the frontend let one through — reported as a typed error
    /// rather than panicking.
    fn innermost_loop(&self) -> Result<&WasmLoop, WasmError> {
        self.loops.last().ok_or(WasmError::JumpOutsideLoop)
    }

    /// Lowers one expression, leaving its value on the stack.
    fn expr(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        id: IrExprId,
    ) -> Result<(), WasmError> {
        match self.program.expr(id) {
            IrExpr::Int(value) => {
                func.i64_const(*value);
            }
            IrExpr::Float(value) => {
                func.f64_const(*value);
            }
            IrExpr::Bool(value) => {
                func.i32_const(i32::from(*value));
            }
            IrExpr::Str(text) => {
                let address = self.literals.intern(text);
                func.addr_const(address);
            }
            IrExpr::Local(slot) => {
                let slot = *slot;
                func.local_get(LocalIdx(slot));
                // Reading a local copies it, as the VM's `LoadLocal` does. Only
                // a struct needs the copy — see `crate::structs` for why a
                // string does not.
                let ty = function
                    .locals
                    .get(slot as usize)
                    .copied()
                    .ok_or_else(|| WasmError::VoidLocal(function.name.clone()))?;
                self.copy_if_mutable(func, ty)?;
            }
            IrExpr::StructNew { struct_id, fields } => {
                let struct_id = *struct_id;
                let fields = fields.clone();
                self.struct_new(func, function, struct_id, &fields)?;
            }
            IrExpr::Field { base, index, ty } => {
                let (base, index, ty) = (*base, *index, *ty);
                let base_ty = self.program.expr_type(function, base);
                let Type::Struct(id) = base_ty else {
                    return Err(WasmError::NotAStruct);
                };
                let offset = u64::from(self.structs.offset(id, index)?);
                let addr = func.addr();
                self.expr(func, function, base)?;
                load_field(func, ty, addr, offset)?;
                // The field is copied out for the same reason a local read is:
                // the base owns it, and handing it out shared would let a write
                // through one be seen through the other.
                self.copy_if_mutable(func, ty)?;
            }
            IrExpr::ArrayNew { ty, elements } => {
                let (ty, elements) = (*ty, elements.clone());
                self.array_new(func, function, ty, &elements)?;
            }
            IrExpr::Index { base, index, ty } => {
                let (base, index, ty) = (*base, *index, *ty);
                let addr = func.addr();
                self.expr(func, function, base)?;
                self.element_slot(func, function, index, ty)?;
                load_field(func, ty, addr, 0)?;
                // Copied out for the same reason a field read is: the array
                // owns its elements.
                self.copy_if_mutable(func, ty)?;
            }
            IrExpr::EnumNew {
                enum_id,
                tag,
                payload,
            } => {
                let (enum_id, tag, payload) = (*enum_id, *tag, *payload);
                self.enum_new(func, function, enum_id, tag, payload)?;
            }
            IrExpr::EnumTag { value } => {
                let value = *value;
                // The enum's box address is on the stack; its tag is the i64 at
                // offset 0. Reading it shares nothing and frees nothing — an
                // enum is immutable and the wasm heap never frees.
                self.expr(func, function, value)?;
                func.i64_load(0);
            }
            IrExpr::EnumPayload { value, ty } => {
                let (value, ty) = (*value, *ty);
                // The box address is on the stack; the payload sits at offset 8,
                // where `enum_new` stored it. Copied out for the same reason a
                // field read is: the box owns its payload, and the binding
                // outlives the box.
                let addr = func.addr();
                self.expr(func, function, value)?;
                load_field(func, ty, addr, 8)?;
                self.copy_if_mutable(func, ty)?;
            }
            IrExpr::ArrayLen { array } => {
                let array = *array;
                self.expr(func, function, array)?;
                func.call(self.runtime.arrays.len);
            }
            IrExpr::ArrayAppend { place, value } => {
                let (place, value) = (place.clone(), *value);
                self.array_append(func, function, &place, value)?;
            }
            IrExpr::Unary { op, operand } => {
                match op {
                    // wasm has no integer negate, and `0 - x` wraps exactly as
                    // the VM's `wrapping_neg` does, including at `Int` minimum.
                    IrUnOp::NegInt => {
                        func.i64_const(0);
                        self.expr(func, function, *operand)?;
                        func.i64_sub();
                    }
                    IrUnOp::NegFloat => {
                        self.expr(func, function, *operand)?;
                        func.f64_neg();
                    }
                    IrUnOp::Not => {
                        self.expr(func, function, *operand)?;
                        func.i32_eqz();
                    }
                }
            }
            IrExpr::Binary { op, lhs, rhs } => self.binary(func, function, *op, *lhs, *rhs)?,
            IrExpr::Call {
                callee,
                args,
                result,
            } => self.call(func, function, *callee, args, *result)?,
        }
        Ok(())
    }

    /// Lowers a call to `print` or to a user function.
    fn call(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        callee: IrCallee,
        args: &[IrExprId],
        result: Type,
    ) -> Result<(), WasmError> {
        let _ = result;
        match callee {
            IrCallee::Print => {
                let Some(argument) = args.first() else {
                    return Err(WasmError::PrintArity(args.len()));
                };
                if args.len() != 1 {
                    return Err(WasmError::PrintArity(args.len()));
                }

                let ty = self.program.expr_type(function, *argument);
                self.expr(func, function, *argument)?;
                // Rendering happens in the module: `print` hands the host bytes,
                // never a number to format.
                match ty {
                    Type::Int(_) => func.call(self.runtime.str_from_i64),
                    Type::Float(_) => func.call(self.runtime.str_from_f64),
                    Type::Bool => func.call(self.runtime.str_from_bool),
                    Type::String => func,
                    Type::Void => {
                        let empty = self.literals.intern("");
                        func.addr_const(empty)
                    }
                    // Analysis rejects printing a struct, an array, or an enum —
                    // none has a rendering the language pins — so a program that
                    // type-checked never reaches this.
                    Type::Struct(_) | Type::Array(_) | Type::Enum(_) => {
                        return Err(WasmError::UnprintableStruct);
                    }
                    Type::Error => return Err(WasmError::ErrorType),
                };
                func.call(self.runtime.print_str);
            }
            IrCallee::User(index) => {
                for argument in args {
                    self.expr(func, function, *argument)?;
                }
                let target = self
                    .functions
                    .get(index as usize)
                    .ok_or(WasmError::UnknownFunction(index))?;
                func.call(*target);
            }
        }
        Ok(())
    }
}
