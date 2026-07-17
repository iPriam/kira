//! Array lowering and the construction-depth walk, split out of the statement
//! and expression lowering in the parent module.
//!
//! Two cohesive jobs live here. The first is everything array-shaped — building
//! a literal, appending, reading and writing an element, and the place walk a
//! nested write threads through fields and indices. The second is the
//! `construction_depth` walk that sizes the scratch locals an object build
//! needs: it must reach *every* expression that could construct one, which is
//! why its matches are exhaustive rather than ending in a wildcard — a node
//! missed there is a scratch local never declared, and so silently wrong
//! codegen rather than a compile error.

use kira_ir::{IrExpr, IrExprId, IrFunction, IrPlace, IrPlaceStep, IrStmt};
use kira_semantics_model::Type;

use crate::error::WasmError;
use crate::func::{Func, LocalIdx};
use crate::structs::{field_size, load_field, store_field};

use super::Lowering;

impl Lowering<'_> {
    /// Copies the value on the stack when its type is one a program can write
    /// through.
    ///
    /// The one rule behind every read here: **the mutable spine is copied and
    /// everything else is shared**. A struct and an array can be written
    /// through, so handing one out shared would let a write through one holder
    /// be seen through the other. A string cannot be written and is never
    /// freed, so sharing it is invisible. Scalars are values already.
    pub(super) fn copy_if_mutable(&mut self, func: &mut Func, ty: Type) -> Result<(), WasmError> {
        match ty {
            Type::Struct(id) => {
                func.call(self.structs.copy(id)?);
            }
            Type::Array(id) => {
                func.call(self.arrays.copy(id)?);
            }
            _ => {}
        }
        Ok(())
    }

    /// Builds an array from its written elements and leaves its address on the
    /// stack.
    ///
    /// Allocate, then fill — the same shape [`Lowering::struct_new`] uses, and
    /// for the same reason: a store wants its destination underneath its value,
    /// so the address is parked in a scratch local while each element is
    /// evaluated. The scratch is indexed by construction depth, so an element
    /// that builds its own object takes the next one down.
    pub(super) fn array_new(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        ty: Type,
        elements: &[IrExprId],
    ) -> Result<(), WasmError> {
        let element = self
            .program
            .types
            .element_of(ty)
            .ok_or(WasmError::NotAnArray)?;
        let addr = func.addr();
        let esize = u64::from(field_size(element, addr)?);
        let depth = self.depth;
        let object = *self.scratch.get(depth).ok_or(WasmError::Wiring)?;
        self.depth += 1;

        let count = u64::try_from(elements.len()).map_err(|_| WasmError::Wiring)?;
        func.addr_const(count).addr_const(esize);
        func.call(self.runtime.arrays.new);
        func.local_set(object);

        for (index, &value) in elements.iter().enumerate() {
            // The element's address, by constant offset: a literal's indices
            // are known here, so this needs no bounds check — the array was
            // allocated with exactly this many slots a moment ago.
            let offset = u64::try_from(index).map_err(|_| WasmError::Wiring)? * esize;
            func.local_get(object);
            func.call(self.runtime.arrays.items);
            func.addr_const(offset);
            func.addr_add();
            self.expr(func, function, value)?;
            store_field(func, element, addr, 0)?;
        }

        func.local_get(object);
        self.depth -= 1;
        Ok(())
    }

    /// Appends one element to the array a place names, leaving nothing on the
    /// stack (`append` yields `Void`).
    pub(super) fn array_append(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        place: &IrPlace,
        value: IrExprId,
    ) -> Result<(), WasmError> {
        // Unlike a store, *every* step is a walk: the place names the array
        // itself, not a slot inside it.
        let ty = self.walk_place(func, function, place.local, &place.path)?;
        let element = self
            .program
            .types
            .element_of(ty)
            .ok_or(WasmError::NotAnArray)?;
        let addr = func.addr();
        func.addr_const(u64::from(field_size(element, addr)?));
        func.call(self.runtime.arrays.push_slot);
        self.expr(func, function, value)?;
        store_field(func, element, addr, 0)?;
        Ok(())
    }

    /// Stores into an assignment target, walking its path.
    ///
    /// The walk loads each intermediate object's address, so the write lands in
    /// the object the local already points at rather than rebuilding it — the
    /// VM's place walk and the native backend's GEP chain do the same.
    ///
    /// An `Index` step is where the two backends differ from a plain field
    /// chain: the address is computed at run time, and the bounds check is
    /// [`crate::rt::array`]'s to make.
    pub(super) fn store_place(
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
        let Some((last, walk)) = place.path.split_last() else {
            return Err(WasmError::Wiring);
        };
        let ty = self.walk_place(func, function, place.local, walk)?;

        // The last step names what to write, rather than what to load.
        match last {
            IrPlaceStep::Field(index) => {
                let Type::Struct(id) = ty else {
                    return Err(WasmError::NotAStruct);
                };
                let offset = u64::from(self.structs.offset(id, *index)?);
                let field_ty = self.field_type(id, *index)?;
                self.expr(func, function, value)?;
                store_field(func, field_ty, addr, offset)?;
            }
            IrPlaceStep::Index(index) => {
                let element = self
                    .program
                    .types
                    .element_of(ty)
                    .ok_or(WasmError::NotAnArray)?;
                // The array's address is already on the stack; turn it into the
                // element's address, which is where the value goes.
                self.element_slot(func, function, *index, element)?;
                self.expr(func, function, value)?;
                store_field(func, element, addr, 0)?;
            }
        }
        Ok(())
    }

    /// Walks `steps` from local `local`, leaving the address they reach on the
    /// stack, and returns its Kira type.
    pub(super) fn walk_place(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        local: u32,
        steps: &[IrPlaceStep],
    ) -> Result<Type, WasmError> {
        let addr = func.addr();
        let mut ty = function
            .locals
            .get(local as usize)
            .copied()
            .ok_or_else(|| WasmError::VoidLocal(function.name.clone()))?;
        func.local_get(LocalIdx(local));

        for step in steps {
            ty = match step {
                IrPlaceStep::Field(index) => {
                    let Type::Struct(id) = ty else {
                        return Err(WasmError::NotAStruct);
                    };
                    let offset = u64::from(self.structs.offset(id, *index)?);
                    let field_ty = self.field_type(id, *index)?;
                    load_field(func, field_ty, addr, offset)?;
                    field_ty
                }
                IrPlaceStep::Index(index) => {
                    let element = self
                        .program
                        .types
                        .element_of(ty)
                        .ok_or(WasmError::NotAnArray)?;
                    self.element_slot(func, function, *index, element)?;
                    load_field(func, element, addr, 0)?;
                    element
                }
            };
        }
        Ok(ty)
    }

    /// Turns the array address on the stack into the address of element
    /// `index`, bounds-checked.
    pub(super) fn element_slot(
        &mut self,
        func: &mut Func,
        function: &IrFunction,
        index: IrExprId,
        element: Type,
    ) -> Result<(), WasmError> {
        let addr = func.addr();
        self.expr(func, function, index)?;
        func.addr_const(u64::from(field_size(element, addr)?));
        func.call(self.runtime.arrays.slot);
        Ok(())
    }

    /// The declared type of one field.
    pub(super) fn field_type(
        &self,
        id: kira_semantics_model::StructId,
        index: u32,
    ) -> Result<Type, WasmError> {
        self.program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.field(index))
            .map(|field| field.ty)
            .ok_or(WasmError::UnknownStruct)
    }

    /// How many levels of nested object construction `body` reaches.
    ///
    /// One scratch local per level is enough because construction is the only
    /// thing that needs one, and a body can only be inside as many at once as
    /// this counts.
    ///
    /// **Both** a struct literal and an array literal take a scratch local, so
    /// both count — and this walk has to reach every expression that could
    /// contain one. A node missed here is not a compile error and not a bad
    /// diagnostic: it is a scratch local that was never declared, which is
    /// silently wrong codegen. That is why the matches below are exhaustive
    /// rather than ending in a wildcard for the nodes that construct nothing.
    pub(super) fn construction_depth(&self, body: &[IrStmt]) -> usize {
        body.iter()
            .map(|stmt| self.stmt_depth(stmt))
            .max()
            .unwrap_or(0)
    }

    pub(super) fn stmt_depth(&self, stmt: &IrStmt) -> usize {
        match stmt {
            IrStmt::Let { init, .. } => self.expr_depth(*init),
            // A place's index expressions are evaluated too, so they count.
            IrStmt::Assign { place, value } => self.place_depth(place).max(self.expr_depth(*value)),
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
            // A jump evaluates nothing, so it constructs nothing.
            IrStmt::Break | IrStmt::Continue => 0,
        }
    }

    /// The deepest construction any of a place's index expressions reaches.
    pub(super) fn place_depth(&self, place: &IrPlace) -> usize {
        place
            .indices()
            .map(|index| self.expr_depth(index))
            .max()
            .unwrap_or(0)
    }

    pub(super) fn expr_depth(&self, id: IrExprId) -> usize {
        match self.program.expr(id) {
            IrExpr::StructNew { fields, .. } => {
                1 + fields
                    .iter()
                    .map(|&field| self.expr_depth(field))
                    .max()
                    .unwrap_or(0)
            }
            // An array literal takes a scratch local exactly as a struct
            // literal does, so it is a level like one.
            IrExpr::ArrayNew { elements, .. } => {
                1 + elements
                    .iter()
                    .map(|&element| self.expr_depth(element))
                    .max()
                    .unwrap_or(0)
            }
            IrExpr::Field { base, .. } => self.expr_depth(*base),
            IrExpr::Index { base, index, .. } => {
                self.expr_depth(*base).max(self.expr_depth(*index))
            }
            IrExpr::ArrayLen { array } => self.expr_depth(*array),
            IrExpr::ArrayAppend { place, value } => {
                self.place_depth(place).max(self.expr_depth(*value))
            }
            IrExpr::Unary { operand, .. } => self.expr_depth(*operand),
            IrExpr::Binary { lhs, rhs, .. } => self.expr_depth(*lhs).max(self.expr_depth(*rhs)),
            IrExpr::Call { args, .. } => args
                .iter()
                .map(|&arg| self.expr_depth(arg))
                .max()
                .unwrap_or(0),
            // Constructs nothing and contains nothing that could.
            IrExpr::Int(_)
            | IrExpr::Float(_)
            | IrExpr::Bool(_)
            | IrExpr::Str(_)
            | IrExpr::Local(_) => 0,
        }
    }
}
