//! Struct layout in linear memory, and the deep copy each struct needs.
//!
//! A struct value is the address of its fields, laid out consecutively. That
//! makes it a pointer like a `String`, so it fits every place a value goes: a
//! wasm local, a parameter, a result.
//!
//! # Why a struct is copied and a string is not
//!
//! The heap here never frees (see [`crate::layout`]), so aliasing is invisible
//! for anything a program cannot mutate — which is why reading a `String` hands
//! back the same address and costs nothing. A struct is different: `p.x = 1`
//! writes through the address. Two values sharing one would see each other's
//! writes, and `var b = a; b.x = 1` changing `a` is exactly what value semantics
//! forbid. So a struct is deep-copied wherever the VM copies one, and the copy
//! is what makes the two engines agree.
//!
//! A `String` *inside* a struct is still shared by that copy: it is immutable
//! and never freed, so nothing can observe the sharing. Only the mutable
//! spine — the struct objects — is duplicated.

use kira_semantics_model::{StructId, StructTable, Type};

use crate::encode::ValType;
use crate::error::WasmError;
use crate::func::{AddrType, FuncIdx};
use crate::module::Module;
use crate::rt::Runtime;

/// Where each field of one struct sits, and how many bytes the whole is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructLayout {
    /// Byte offset of each field, in declaration order.
    pub offsets: Vec<u32>,
    /// The object's total size in bytes.
    pub size: u32,
}

/// Every struct's layout and copy helper, for one address width.
#[derive(Debug, Clone)]
pub struct Structs {
    layouts: Vec<StructLayout>,
    copies: Vec<FuncIdx>,
}

impl Structs {
    /// Lays out every declared struct and reserves one copy helper per struct.
    ///
    /// Every index is reserved before any body exists, so a copy helper can
    /// call the helper of a struct it contains — the same reason the runtime
    /// declares its own helpers up front.
    pub fn declare(module: &mut Module, table: &StructTable) -> Result<Self, WasmError> {
        let addr = module.addr();
        let mut layouts = Vec::with_capacity(table.len());
        let mut copies = Vec::with_capacity(table.len());
        for def in table.defs() {
            let mut offsets = Vec::with_capacity(def.fields.len());
            let mut cursor = 0u32;
            for field in &def.fields {
                let size = field_size(field.ty, addr)?;
                // Natural alignment: a field starts at a multiple of its own
                // size. Wasm permits unaligned access, so this buys speed
                // rather than correctness — but it also means the offsets are
                // the ones a reader would predict.
                cursor = cursor.next_multiple_of(size);
                offsets.push(cursor);
                cursor += size;
            }
            layouts.push(StructLayout {
                offsets,
                size: cursor.max(1),
            });
            copies.push(module.declare(vec![addr.val()], vec![addr.val()]));
        }
        Ok(Self { layouts, copies })
    }

    /// The layout of a declared struct.
    pub fn layout(&self, id: StructId) -> Result<&StructLayout, WasmError> {
        self.layouts
            .get(id.index() as usize)
            .ok_or(WasmError::UnknownStruct)
    }

    /// The byte offset of one field.
    pub fn offset(&self, id: StructId, index: u32) -> Result<u32, WasmError> {
        self.layout(id)?
            .offsets
            .get(index as usize)
            .copied()
            .ok_or(WasmError::UnknownStruct)
    }

    /// The copy helper for a declared struct: `copy(address) -> address`.
    pub fn copy(&self, id: StructId) -> Result<FuncIdx, WasmError> {
        self.copies
            .get(id.index() as usize)
            .copied()
            .ok_or(WasmError::UnknownStruct)
    }

    /// Emits every copy helper's body.
    ///
    /// `arrays` is needed because an **array field is mutable**, so a struct's
    /// copy has to copy it — sharing the handle would let a write through one
    /// struct be seen through the other, which is exactly what value semantics
    /// forbid. A `String` field is still shared, per this module's docs: the
    /// line is mutability, not heap-ness.
    pub fn define(
        &self,
        module: &mut Module,
        rt: &Runtime,
        table: &StructTable,
        arrays: &crate::arrays::ArrayCopies,
    ) -> Result<(), WasmError> {
        for (index, def) in table.defs().iter().enumerate() {
            let id = struct_id_at(table, index)?;
            let layout = self.layout(id)?.clone();
            let addr = module.addr();
            let mut func = module.func(vec![addr.val()], vec![addr.val()]);
            let Some(source) = func.param(0) else {
                return Err(WasmError::Wiring);
            };
            let object = func.local(addr.val());

            // Allocate the copy first: every field store needs its address.
            // `local_set` rather than `local_tee` — each store pushes its own
            // copy of the address, so leaving one here would strand it on the
            // stack at the end of the body.
            func.i32_const(layout.size as i32).i32_to_addr();
            func.call(rt.alloc);
            func.local_set(object);

            for (field_index, field) in def.fields.iter().enumerate() {
                let offset = layout.offsets[field_index] as u64;
                // Destination address for the store.
                func.local_get(object);
                // Read the field out of the source.
                func.local_get(source);
                load_field(&mut func, field.ty, addr, offset)?;
                // A nested struct and an array are the mutable parts, so both
                // are copied; a string is shared, per this module's docs.
                match field.ty {
                    Type::Struct(inner) => {
                        func.call(self.copy(inner)?);
                    }
                    Type::Array(inner) => {
                        func.call(arrays.copy(inner)?);
                    }
                    _ => {}
                }
                store_field(&mut func, field.ty, addr, offset)?;
            }

            func.local_get(object);
            if !module.define(self.copy(id)?, func) {
                return Err(WasmError::Wiring);
            }
        }
        Ok(())
    }
}

/// The `StructId` of the struct at `index` in declaration order.
///
/// The table mints ids in declaration order and hands out no other way to name
/// one, so looking the id back up by name is total rather than a search that
/// might miss.
fn struct_id_at(table: &StructTable, index: usize) -> Result<StructId, WasmError> {
    table
        .defs()
        .get(index)
        .and_then(|def| table.lookup(&def.name))
        .ok_or(WasmError::UnknownStruct)
}

/// How many bytes a field of `ty` occupies.
pub fn field_size(ty: Type, addr: AddrType) -> Result<u32, WasmError> {
    Ok(match ty {
        Type::Int | Type::Float => 8,
        Type::Bool => 4,
        // A pointer, as wide as the memory is. An array is one too: its
        // value is its header's address.
        Type::String | Type::Struct(_) | Type::Array(_) => match addr.val() {
            ValType::I64 => 8,
            _ => 4,
        },
        Type::Void => return Err(WasmError::VoidField),
        Type::Error => return Err(WasmError::ErrorType),
    })
}

/// Emits a load of a field at `offset`, with the base address on the stack.
pub fn load_field(
    func: &mut crate::func::Func,
    ty: Type,
    addr: AddrType,
    offset: u64,
) -> Result<(), WasmError> {
    match ty {
        Type::Int => {
            func.i64_load(offset);
        }
        Type::Float => {
            func.f64_load(offset);
        }
        Type::Bool => {
            func.i32_load(offset);
        }
        Type::String | Type::Struct(_) | Type::Array(_) => match addr.val() {
            ValType::I64 => {
                func.i64_load(offset);
            }
            _ => {
                func.i32_load(offset);
            }
        },
        Type::Void => return Err(WasmError::VoidField),
        Type::Error => return Err(WasmError::ErrorType),
    }
    Ok(())
}

/// Emits a store of a field at `offset`, with the base address and then the
/// value on the stack.
pub fn store_field(
    func: &mut crate::func::Func,
    ty: Type,
    addr: AddrType,
    offset: u64,
) -> Result<(), WasmError> {
    match ty {
        Type::Int => {
            func.i64_store(offset);
        }
        Type::Float => {
            func.f64_store(offset);
        }
        Type::Bool => {
            func.i32_store(offset);
        }
        Type::String | Type::Struct(_) | Type::Array(_) => match addr.val() {
            ValType::I64 => {
                func.i64_store(offset);
            }
            _ => {
                func.i32_store(offset);
            }
        },
        Type::Void => return Err(WasmError::VoidField),
        Type::Error => return Err(WasmError::ErrorType),
    }
    Ok(())
}
