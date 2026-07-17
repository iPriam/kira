//! The deep copy each array type needs.
//!
//! The allocation, bounds checking, and growth are generic and live in
//! [`crate::rt::array`]. Only the copy is per-type, and only because it has to
//! **recurse**: copying a `[[Point]]` means copying each `[Point]`, which means
//! copying each `Point`. Element size alone cannot say that, so this is where a
//! helper per array type earns its place.
//!
//! # What gets copied, and what is shared
//!
//! The same line [`crate::structs`] draws: the **mutable spine** is duplicated
//! and everything immutable is shared. A `String` element is shared — it cannot
//! be written and is never freed, so nothing can observe the sharing. A struct
//! or array element *is* mutable, so it is copied, or a write through one copy
//! would be visible through the other.
//!
//! # Why the copy is flat first and deep second
//!
//! Every copy starts as one `memcpy` of the whole item block. For an array of
//! scalars or strings that is the entire job. For an array of structs it is a
//! head start: the block then holds the *source's* element pointers, and the
//! loop replaces each with a copy. Doing it this way means one code path
//! instead of two, and the loop is emitted only for the element types that
//! need it.

use kira_semantics_model::{ArrayId, ArrayTable, Type};

use crate::error::WasmError;
use crate::func::{BlockType, FuncIdx};
use crate::module::Module;
use crate::rt::Runtime;
use crate::rt::array::{HeaderLayout, LEN_OFFSET};
use crate::structs::{Structs, field_size};

/// One deep-copy helper per array type the program mentions.
#[derive(Debug, Clone)]
pub struct ArrayCopies {
    copies: Vec<FuncIdx>,
}

impl ArrayCopies {
    /// Reserves one copy helper per array type.
    ///
    /// Every index is reserved before any body exists, so an array's copy can
    /// call the copy of the array or struct it holds — the same reason the
    /// runtime and the struct copies declare up front.
    pub fn declare(module: &mut Module, table: &ArrayTable) -> Self {
        let addr = module.addr().val();
        let copies = (0..table.len())
            .map(|_| module.declare(vec![addr], vec![addr]))
            .collect();
        Self { copies }
    }

    /// The copy helper for an array type: `copy(header) -> header`.
    pub fn copy(&self, id: ArrayId) -> Result<FuncIdx, WasmError> {
        self.copies
            .get(id.index() as usize)
            .copied()
            .ok_or(WasmError::UnknownArray)
    }

    /// Emits every copy helper's body.
    pub fn define(
        &self,
        module: &mut Module,
        rt: &Runtime,
        table: &ArrayTable,
        structs: &Structs,
    ) -> Result<(), WasmError> {
        for (id, element) in table.rows().collect::<Vec<_>>() {
            self.define_one(module, rt, id, element, structs)?;
        }
        Ok(())
    }

    fn define_one(
        &self,
        module: &mut Module,
        rt: &Runtime,
        id: ArrayId,
        element: Type,
        structs: &Structs,
    ) -> Result<(), WasmError> {
        let addr_ty = module.addr();
        let addr = addr_ty.val();
        let header = HeaderLayout::of(addr_ty);
        let esize = u64::from(field_size(element, addr_ty)?);

        let mut func = module.func(vec![addr], vec![addr]);
        let Some(source) = func.param(0) else {
            return Err(WasmError::Wiring);
        };
        let count = func.local(addr);
        let object = func.local(addr);

        func.local_get(source)
            .addr_load(u64::from(LEN_OFFSET))
            .local_set(count);

        // A fresh array of the same length, then one flat copy of the items.
        func.local_get(count)
            .addr_const(esize)
            .call(rt.arrays.new)
            .local_set(object);
        func.local_get(object).addr_load(header.items);
        func.local_get(source).addr_load(header.items);
        // The byte count is an `i32`: `memcpy`'s existing limit, not a new one.
        func.local_get(count)
            .addr_const(esize)
            .addr_mul()
            .addr_to_i32();
        func.call(rt.memcpy);

        // The block now holds the source's own element pointers. For a mutable
        // element that is aliasing, so each one is replaced by its copy.
        if let Some(element_copy) = self.element_copy(element, structs)? {
            let cursor = func.local(addr);
            let slot = func.local(addr);
            func.addr_const(0).local_set(cursor);

            func.block(BlockType::Empty);
            func.loop_(BlockType::Empty);
            {
                // Leave when the cursor reaches the count.
                func.local_get(cursor).local_get(count).addr_eq();
                func.br_if(1);

                // slot = items + cursor * esize
                func.local_get(object).addr_load(header.items);
                func.local_get(cursor).addr_const(esize).addr_mul();
                func.addr_add();
                func.local_set(slot);

                // *slot = copy(*slot)
                func.local_get(slot);
                func.local_get(slot).addr_load(0);
                func.call(element_copy);
                func.addr_store(0);

                func.local_get(cursor).addr_const(1).addr_add();
                func.local_set(cursor);
                func.br(0);
            }
            func.end();
            func.end();
        }

        func.local_get(object);
        if !module.define(self.copy(id)?, func) {
            return Err(WasmError::Wiring);
        }
        Ok(())
    }

    /// The copy helper an element of `ty` needs, or `None` when the flat copy
    /// already got it right.
    fn element_copy(&self, ty: Type, structs: &Structs) -> Result<Option<FuncIdx>, WasmError> {
        Ok(match ty {
            Type::Struct(inner) => Some(structs.copy(inner)?),
            Type::Array(inner) => Some(self.copy(inner)?),
            // A scalar is its own copy, and a string is shared — see this
            // module's docs.
            _ => None,
        })
    }
}
