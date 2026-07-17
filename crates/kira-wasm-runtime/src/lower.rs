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

use kira_ir::{
    IrBinOp, IrCallee, IrExpr, IrExprId, IrFunction, IrPlace, IrProgram, IrStmt, IrUnOp,
};
use kira_semantics_model::Type;

use crate::encode::ValType;
use crate::error::WasmError;
use crate::func::{BlockType, BlockType::Empty, Func, FuncIdx, LocalIdx};
use crate::literals::Literals;
use crate::rt::Runtime;
use crate::structs::{Structs, load_field, store_field};

/// Lowers one function at a time against a fixed set of handles.
pub struct Lowering<'a> {
    program: &'a IrProgram,
    runtime: &'a Runtime,
    literals: &'a mut Literals,
    functions: &'a [FuncIdx],
    structs: &'a Structs,
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
    ) -> Self {
        Self {
            program,
            runtime,
            literals,
            functions,
            structs,
            scratch: Vec::new(),
            depth: 0,
        }
    }

    /// The wasm value type a Kira type occupies, or `None` for `Void`.
    ///
    /// `Void` has no representation: a `Void` expression leaves nothing on the
    /// stack, which is why nothing has to be dropped after a call to a function
    /// that returns nothing.
    pub fn val_type(ty: Type) -> Result<Option<ValType>, WasmError> {
        Ok(match ty {
            Type::Int => Some(ValType::I64),
            Type::Float => Some(ValType::F64),
            Type::Bool => Some(ValType::I32),
            // Both are addresses; `value_of` widens them to the memory's width.
            Type::String | Type::Struct(_) => Some(ValType::I32),
            Type::Void => None,
            Type::Error => return Err(WasmError::ErrorType),
        })
    }

    /// The wasm value type a Kira type occupies, resolving `String` against the
    /// module's address width.
    fn value_of(&self, ty: Type, addr: ValType) -> Result<Option<ValType>, WasmError> {
        Ok(match Self::val_type(ty)? {
            // A `String` and a struct are addresses, so both are as wide as the
            // memory is.
            Some(ValType::I32) if matches!(ty, Type::String | Type::Struct(_)) => Some(addr),
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

    /// Stores into an assignment target, walking its field path.
    ///
    /// The walk loads each intermediate struct's address, so the write lands in
    /// the object the local already points at rather than rebuilding it — the
    /// VM's `StoreField` and the native backend's GEP chain do the same.
    fn store_place(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        place: &IrPlace,
        value: IrExprId,
    ) -> Result<(), WasmError> {
        if place.path.is_empty() {
            self.expr(func, function, value)?;
            func.local_set(LocalIdx(place.local));
            return Ok(());
        }

        let addr = func.addr();
        let mut ty = function
            .locals
            .get(place.local as usize)
            .copied()
            .ok_or_else(|| WasmError::VoidLocal(function.name.clone()))?;
        func.local_get(LocalIdx(place.local));

        // Every step but the last loads the next object's address; the last one
        // names the field to write.
        let Some((&last, walk)) = place.path.split_last() else {
            return Err(WasmError::Wiring);
        };
        for &index in walk {
            let Type::Struct(id) = ty else {
                return Err(WasmError::NotAStruct);
            };
            let offset = u64::from(self.structs.offset(id, index)?);
            let field_ty = self.field_type(id, index)?;
            load_field(func, field_ty, addr, offset)?;
            ty = field_ty;
        }

        let Type::Struct(id) = ty else {
            return Err(WasmError::NotAStruct);
        };
        let offset = u64::from(self.structs.offset(id, last)?);
        let field_ty = self.field_type(id, last)?;
        self.expr(func, function, value)?;
        store_field(func, field_ty, addr, offset)?;
        Ok(())
    }

    /// The declared type of one field.
    fn field_type(
        &self,
        id: kira_semantics_model::StructId,
        index: u32,
    ) -> Result<Type, WasmError> {
        self.program
            .structs
            .get(id)
            .and_then(|def| def.field(index))
            .map(|field| field.ty)
            .ok_or(WasmError::UnknownStruct)
    }

    /// How many levels of nested struct construction `body` reaches.
    ///
    /// One scratch local per level is enough because construction is the only
    /// thing that needs one, and a body can only be inside as many at once as
    /// this counts.
    fn construction_depth(&self, body: &[IrStmt]) -> usize {
        body.iter()
            .map(|stmt| self.stmt_depth(stmt))
            .max()
            .unwrap_or(0)
    }

    fn stmt_depth(&self, stmt: &IrStmt) -> usize {
        match stmt {
            IrStmt::Let { init, .. } => self.expr_depth(*init),
            IrStmt::Assign { value, .. } => self.expr_depth(*value),
            IrStmt::Return { value } => value.map_or(0, |expr| self.expr_depth(expr)),
            IrStmt::Eval { expr } => self.expr_depth(*expr),
            IrStmt::If {
                cond,
                then_body,
                else_body,
            } => self
                .expr_depth(*cond)
                .max(self.construction_depth(then_body))
                .max(self.construction_depth(else_body)),
            IrStmt::While { cond, body } => {
                self.expr_depth(*cond).max(self.construction_depth(body))
            }
        }
    }

    fn expr_depth(&self, id: IrExprId) -> usize {
        match self.program.expr(id) {
            IrExpr::StructNew { fields, .. } => {
                1 + fields
                    .iter()
                    .map(|&field| self.expr_depth(field))
                    .max()
                    .unwrap_or(0)
            }
            IrExpr::Field { base, .. } => self.expr_depth(*base),
            IrExpr::Unary { operand, .. } => self.expr_depth(*operand),
            IrExpr::Binary { lhs, rhs, .. } => self.expr_depth(*lhs).max(self.expr_depth(*rhs)),
            IrExpr::Call { args, .. } => args
                .iter()
                .map(|&arg| self.expr_depth(arg))
                .max()
                .unwrap_or(0),
            _ => 0,
        }
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
                self.body(func, function, then_body)?;
                if !else_body.is_empty() {
                    func.else_();
                    self.body(func, function, else_body)?;
                }
                func.end();
            }
            IrStmt::While { cond, body } => {
                // block { loop { br_if 1 (!cond); body; br 0 } }: the condition
                // is tested before each iteration, including the first.
                func.block(Empty);
                func.loop_(Empty);
                self.expr(func, function, *cond)?;
                func.i32_eqz().br_if(1);
                self.body(func, function, body)?;
                func.br(0);
                func.end();
                func.end();
            }
        }
        Ok(())
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
                if let Type::Struct(id) = ty {
                    func.call(self.structs.copy(id)?);
                }
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
                if let Type::Struct(inner) = ty {
                    func.call(self.structs.copy(inner)?);
                }
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

    /// Lowers a binary operation.
    fn binary(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), WasmError> {
        // `&&` and `||` decide whether the right operand runs at all, so they
        // are branches rather than operators.
        match op {
            IrBinOp::And => {
                self.expr(func, function, lhs)?;
                func.if_(BlockType::Value(ValType::I32));
                self.expr(func, function, rhs)?;
                func.else_();
                func.i32_const(0);
                func.end();
                return Ok(());
            }
            IrBinOp::Or => {
                self.expr(func, function, lhs)?;
                func.if_(BlockType::Value(ValType::I32));
                func.i32_const(1);
                func.else_();
                self.expr(func, function, rhs)?;
                func.end();
                return Ok(());
            }
            IrBinOp::DivInt | IrBinOp::RemInt => {
                return self.int_division(func, function, op, lhs, rhs);
            }
            _ => {}
        }

        self.expr(func, function, lhs)?;
        self.expr(func, function, rhs)?;

        match op {
            IrBinOp::AddInt => func.i64_add(),
            IrBinOp::SubInt => func.i64_sub(),
            IrBinOp::MulInt => func.i64_mul(),
            IrBinOp::AddFloat => func.f64_add(),
            IrBinOp::SubFloat => func.f64_sub(),
            IrBinOp::MulFloat => func.f64_mul(),
            IrBinOp::DivFloat => func.f64_div(),
            IrBinOp::EqInt => func.i64_eq(),
            IrBinOp::NeInt => func.i64_ne(),
            IrBinOp::LtInt => func.i64_lt_s(),
            IrBinOp::LeInt => func.i64_le_s(),
            IrBinOp::GtInt => func.i64_gt_s(),
            IrBinOp::GeInt => func.i64_ge_s(),
            IrBinOp::EqFloat => func.f64_eq(),
            IrBinOp::NeFloat => func.f64_ne(),
            IrBinOp::LtFloat => func.f64_lt(),
            IrBinOp::LeFloat => func.f64_le(),
            IrBinOp::GtFloat => func.f64_gt(),
            IrBinOp::GeFloat => func.f64_ge(),
            IrBinOp::EqBool => func.i32_eq(),
            IrBinOp::NeBool => func.i32_ne(),
            IrBinOp::ConcatStr => func.call(self.runtime.str_concat),
            IrBinOp::EqStr => func.call(self.runtime.str_eq),
            IrBinOp::NeStr => func.call(self.runtime.str_eq).i32_eqz(),
            // Handled above; listed so a new operator cannot fall through.
            IrBinOp::And | IrBinOp::Or | IrBinOp::DivInt | IrBinOp::RemInt => {
                return Err(WasmError::UnsupportedOperator);
            }
        };
        Ok(())
    }

    /// Lowers `/` or `%` on integers, with Kira's answers for the two cases
    /// wasm would decide differently.
    fn int_division(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        op: IrBinOp,
        lhs: IrExprId,
        rhs: IrExprId,
    ) -> Result<(), WasmError> {
        let left = func.local(ValType::I64);
        let right = func.local(ValType::I64);

        self.expr(func, function, lhs)?;
        self.expr(func, function, rhs)?;
        func.local_set(right);
        func.local_set(left);

        // By zero is a Kira trap, and it is raised before the engine can raise
        // its own — so a Web user reads the same sentence a VM user does.
        func.local_get(right).i64_eqz();
        func.if_(Empty);
        func.call(self.runtime.trap_div_zero).unreachable();
        func.end();

        // `Int::MIN / -1` overflows: wasm traps, the VM wraps to `Int::MIN`,
        // and `Int::MIN % -1` is zero rather than a trap.
        func.local_get(left)
            .i64_const(i64::MIN)
            .i64_eq()
            .local_get(right)
            .i64_const(-1)
            .i64_eq()
            .i32_and();
        func.if_(BlockType::Value(ValType::I64));
        match op {
            IrBinOp::DivInt => func.i64_const(i64::MIN),
            _ => func.i64_const(0),
        };
        func.else_();
        func.local_get(left).local_get(right);
        match op {
            IrBinOp::DivInt => func.i64_div_s(),
            _ => func.i64_rem_s(),
        };
        func.end();
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
                    Type::Int => func.call(self.runtime.str_from_i64),
                    Type::Float => func.call(self.runtime.str_from_f64),
                    Type::Bool => func.call(self.runtime.str_from_bool),
                    Type::String => func,
                    Type::Void => {
                        let empty = self.literals.intern("");
                        func.addr_const(empty)
                    }
                    // Analysis rejects printing a struct — what it renders is
                    // not pinned by the language — so a program that
                    // type-checked never reaches this.
                    Type::Struct(_) => return Err(WasmError::UnprintableStruct),
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
