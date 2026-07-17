//! The function body builder: the instruction surface the rest of the backend
//! writes against.
//!
//! Instructions are emitted straight to bytes rather than buffered as a tree.
//! The builder tracks nothing but the local declarations and the code, so it
//! stays honest about what it is: a byte sink with names on the opcodes.
//! Structural correctness (matched `end`s, stack shape) is the caller's, and
//! the tests hold a real engine to it.

use crate::encode::{Bytes, ValType};

mod num;
pub mod op;

/// An index into a module's function space (imports first, then definitions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FuncIdx(pub u32);

/// An index into a function's local space (parameters first).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LocalIdx(pub u32);

/// An index into a module's global space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GlobalIdx(pub u32);

/// The result shape of a `block`, `loop`, or `if`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// The block leaves nothing on the stack.
    Empty,
    /// The block leaves one value of this type.
    Value(ValType),
}

impl BlockType {
    /// The type's encoding in a block header.
    fn code(self) -> u8 {
        match self {
            Self::Empty => 0x40,
            Self::Value(ty) => ty.code(),
        }
    }
}

/// How wide a linear-memory address is.
///
/// This is the one axis Memory64 moves. Every pointer the backend emits — a
/// `String` value, an allocation, a big-integer register — is an address, so
/// the width is threaded through the builder and spent through the `addr_*`
/// instructions rather than being spelled `i32` at each of the hundreds of
/// places a pointer is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AddrType {
    /// 32-bit addresses: the baseline `wasm32` memory.
    I32,
    /// 64-bit addresses: the Memory64 proposal's `wasm64` memory.
    I64,
}

impl AddrType {
    /// The value type an address of this width occupies.
    pub fn val(self) -> ValType {
        match self {
            Self::I32 => ValType::I32,
            Self::I64 => ValType::I64,
        }
    }
}

/// Builds one function body.
#[derive(Debug)]
pub struct Func {
    addr: AddrType,
    params: Vec<ValType>,
    results: Vec<ValType>,
    locals: Vec<ValType>,
    code: Bytes,
}

impl Func {
    /// Starts a function with the given address width and signature.
    pub fn new(addr: AddrType, params: Vec<ValType>, results: Vec<ValType>) -> Self {
        Self {
            addr,
            params,
            results,
            locals: Vec::new(),
            code: Bytes::new(),
        }
    }

    /// The address width this body is emitted for.
    pub fn addr(&self) -> AddrType {
        self.addr
    }

    /// The function's parameter types.
    pub fn params(&self) -> &[ValType] {
        &self.params
    }

    /// The function's result types.
    pub fn results(&self) -> &[ValType] {
        &self.results
    }

    /// Declares one more local, returning its index.
    ///
    /// Locals follow the parameters in the same index space, which is why the
    /// index is handed back rather than computed by the caller.
    pub fn local(&mut self, ty: ValType) -> LocalIdx {
        let index = self.params.len() + self.locals.len();
        self.locals.push(ty);
        LocalIdx(index as u32)
    }

    /// The index of parameter `position`.
    ///
    /// Returns `None` past the last parameter, so a caller cannot silently
    /// address a local as though it were an argument.
    pub fn param(&self, position: u32) -> Option<LocalIdx> {
        (position < self.params.len() as u32).then_some(LocalIdx(position))
    }

    /// Encodes the body as a code-section entry: local groups, then the code,
    /// then the terminating `end`.
    pub fn finish(self) -> Bytes {
        let mut body = Bytes::new();

        // Locals are run-length encoded by type; the runs come out of the
        // declaration order, which is also the index order.
        let mut runs: Vec<(u32, ValType)> = Vec::new();
        for ty in &self.locals {
            match runs.last_mut() {
                Some((count, last)) if last == ty => *count += 1,
                _ => runs.push((1, *ty)),
            }
        }
        body.u32(runs.len() as u32);
        for (count, ty) in runs {
            body.u32(count);
            body.byte(ty.code());
        }

        body.raw(self.code.as_slice());
        body.byte(op::END);

        let mut entry = Bytes::new();
        entry.sized(&body);
        entry
    }
}

/// Control flow.
impl Func {
    /// Emits `block`.
    pub fn block(&mut self, ty: BlockType) -> &mut Self {
        self.code.byte(op::BLOCK);
        self.code.byte(ty.code());
        self
    }

    /// Emits `loop`.
    pub fn loop_(&mut self, ty: BlockType) -> &mut Self {
        self.code.byte(op::LOOP);
        self.code.byte(ty.code());
        self
    }

    /// Emits `if`, consuming a condition.
    pub fn if_(&mut self, ty: BlockType) -> &mut Self {
        self.code.byte(op::IF);
        self.code.byte(ty.code());
        self
    }

    /// Emits `else`.
    pub fn else_(&mut self) -> &mut Self {
        self.code.byte(op::ELSE);
        self
    }

    /// Emits `end`, closing the innermost open block.
    pub fn end(&mut self) -> &mut Self {
        self.code.byte(op::END);
        self
    }

    /// Emits `br` to the block `depth` levels out.
    pub fn br(&mut self, depth: u32) -> &mut Self {
        self.code.byte(op::BR);
        self.code.u32(depth);
        self
    }

    /// Emits `br_if` to the block `depth` levels out.
    pub fn br_if(&mut self, depth: u32) -> &mut Self {
        self.code.byte(op::BR_IF);
        self.code.u32(depth);
        self
    }

    /// Emits `return`.
    pub fn return_(&mut self) -> &mut Self {
        self.code.byte(op::RETURN);
        self
    }

    /// Emits `unreachable`.
    pub fn unreachable(&mut self) -> &mut Self {
        self.code.byte(op::UNREACHABLE);
        self
    }

    /// Emits `drop`.
    pub fn drop(&mut self) -> &mut Self {
        self.code.byte(op::DROP);
        self
    }

    /// Emits `call`.
    pub fn call(&mut self, func: FuncIdx) -> &mut Self {
        self.code.byte(op::CALL);
        self.code.u32(func.0);
        self
    }
}

/// Locals, globals, and constants.
impl Func {
    /// Emits `local.get`.
    pub fn local_get(&mut self, local: LocalIdx) -> &mut Self {
        self.code.byte(op::LOCAL_GET);
        self.code.u32(local.0);
        self
    }

    /// Emits `local.set`.
    pub fn local_set(&mut self, local: LocalIdx) -> &mut Self {
        self.code.byte(op::LOCAL_SET);
        self.code.u32(local.0);
        self
    }

    /// Emits `local.tee`.
    pub fn local_tee(&mut self, local: LocalIdx) -> &mut Self {
        self.code.byte(op::LOCAL_TEE);
        self.code.u32(local.0);
        self
    }

    /// Emits `global.get`.
    pub fn global_get(&mut self, global: GlobalIdx) -> &mut Self {
        self.code.byte(op::GLOBAL_GET);
        self.code.u32(global.0);
        self
    }

    /// Emits `global.set`.
    pub fn global_set(&mut self, global: GlobalIdx) -> &mut Self {
        self.code.byte(op::GLOBAL_SET);
        self.code.u32(global.0);
        self
    }

    /// Emits `i32.const`.
    pub fn i32_const(&mut self, value: i32) -> &mut Self {
        self.code.byte(op::I32_CONST);
        self.code.i32(value);
        self
    }

    /// Emits `i64.const`.
    pub fn i64_const(&mut self, value: i64) -> &mut Self {
        self.code.byte(op::I64_CONST);
        self.code.i64(value);
        self
    }

    /// Emits `f64.const`.
    pub fn f64_const(&mut self, value: f64) -> &mut Self {
        self.code.byte(op::F64_CONST);
        self.code.f64(value);
        self
    }
}

/// Linear memory access.
///
/// Every access the backend emits is naturally aligned, so the alignment hint
/// is derived from the width rather than passed in and got wrong. Each takes
/// its address from the stack in this body's [`AddrType`]; the *value* loaded
/// or stored keeps its own width, which is why a length word is still `i32`
/// under Memory64.
impl Func {
    /// Emits `i32.load` at `offset`.
    pub fn i32_load(&mut self, offset: u64) -> &mut Self {
        self.mem(op::I32_LOAD, 2, offset)
    }

    /// Emits `i32.load8_u` at `offset`.
    pub fn i32_load8_u(&mut self, offset: u64) -> &mut Self {
        self.mem(op::I32_LOAD8_U, 0, offset)
    }

    /// Emits `i32.store` at `offset`.
    pub fn i32_store(&mut self, offset: u64) -> &mut Self {
        self.mem(op::I32_STORE, 2, offset)
    }

    /// Emits `i32.store8` at `offset`.
    pub fn i32_store8(&mut self, offset: u64) -> &mut Self {
        self.mem(op::I32_STORE8, 0, offset)
    }

    /// Emits `memory.size`, in 64KiB pages, as an address-width count.
    pub fn memory_size(&mut self) -> &mut Self {
        self.code.byte(op::MEMORY_SIZE);
        self.code.byte(0x00);
        self
    }

    /// Emits `memory.grow` by an address-width count of 64KiB pages.
    pub fn memory_grow(&mut self) -> &mut Self {
        self.code.byte(op::MEMORY_GROW);
        self.code.byte(0x00);
        self
    }

    /// Emits `memory.copy`, taking destination, source, and length from the
    /// stack at this body's address width.
    ///
    /// The bulk-memory instruction rather than a byte loop. It is not an
    /// optimisation of the module so much as of every string a program builds:
    /// concatenation is a copy, and a copy that runs one byte per loop
    /// iteration makes repeated concatenation quadratic in interpreted work as
    /// well as in bytes.
    pub fn memory_copy(&mut self) -> &mut Self {
        self.code.byte(op::PREFIX_FC);
        self.code.u32(op::MEMORY_COPY);
        // Destination and source memory indices: a module has one memory.
        self.code.byte(0x00);
        self.code.byte(0x00);
        self
    }

    /// Emits a memory instruction with its alignment hint and offset.
    ///
    /// The static offset is a `u64` under Memory64 and a `u32` under Memory32;
    /// the format encodes both as an unsigned LEB, so the wider encoder is
    /// correct for either.
    fn mem(&mut self, opcode: u8, align: u32, offset: u64) -> &mut Self {
        self.code.byte(opcode);
        self.code.u32(align);
        self.code.u64(offset);
        self
    }
}

/// Address arithmetic, emitted at this body's [`AddrType`].
///
/// These are the only instructions that touch a pointer. Under Memory32 each
/// spells its `i32` form and under Memory64 its `i64` form, so the emitters
/// above them are written once and are correct on both memories.
impl Func {
    /// Pushes a constant address.
    pub fn addr_const(&mut self, value: u64) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_const(value as i32),
            AddrType::I64 => self.i64_const(value as i64),
        }
    }

    /// Emits an address `add`.
    pub fn addr_add(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_add(),
            AddrType::I64 => self.i64_add(),
        }
    }

    /// Emits an address `sub`.
    pub fn addr_sub(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_sub(),
            AddrType::I64 => self.i64_sub(),
        }
    }

    /// Emits an address `mul`.
    pub fn addr_mul(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_mul(),
            AddrType::I64 => self.i64_mul(),
        }
    }

    /// Emits an address `eq`.
    pub fn addr_eq(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_eq(),
            AddrType::I64 => self.i64_eq(),
        }
    }

    /// Emits an unsigned address `gt`.
    pub fn addr_gt_u(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_gt_u(),
            AddrType::I64 => self.i64_gt_u(),
        }
    }

    /// Emits an address `and`.
    pub fn addr_and(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_and(),
            AddrType::I64 => self.i64_and(),
        }
    }

    /// Emits an unsigned address `div`.
    pub fn addr_div_u(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self.i32_div_u(),
            AddrType::I64 => self.i64_div_u(),
        }
    }

    /// Converts an `i32` on the stack to an address, zero-extending under
    /// Memory64.
    ///
    /// This is the seam between a count and a pointer: a string's length word
    /// is 32 bits on both memories, and offsetting a pointer by it needs the
    /// address width.
    pub fn i32_to_addr(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self,
            AddrType::I64 => self.i64_extend_i32_u(),
        }
    }

    /// Converts an address on the stack to an `i32`, truncating under Memory64.
    ///
    /// Only used where the value is a length or a difference already known to
    /// fit — a string's byte count — never to narrow a live pointer.
    pub fn addr_to_i32(&mut self) -> &mut Self {
        match self.addr {
            AddrType::I32 => self,
            AddrType::I64 => self.i32_wrap_i64(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_body_is_no_locals_and_an_end() {
        let func = Func::new(AddrType::I32, Vec::new(), Vec::new());
        // One length byte, one zero local-group count, one `end`.
        assert_eq!(func.finish().as_slice(), &[0x02, 0x00, op::END]);
    }

    #[test]
    fn an_address_op_follows_the_bodys_width() {
        let mut narrow = Func::new(AddrType::I32, Vec::new(), Vec::new());
        narrow.addr_const(8).addr_add();
        let mut wide = Func::new(AddrType::I64, Vec::new(), Vec::new());
        wide.addr_const(8).addr_add();

        let narrow = narrow.finish().into_vec();
        let wide = wide.finish().into_vec();
        assert!(narrow.contains(&op::I32_ADD) && !narrow.contains(&op::I64_ADD));
        assert!(wide.contains(&op::I64_ADD) && !wide.contains(&op::I32_ADD));
    }

    #[test]
    fn widening_a_count_to_an_address_is_free_on_memory32_only() {
        let mut narrow = Func::new(AddrType::I32, Vec::new(), Vec::new());
        narrow.i32_const(1).i32_to_addr();
        let mut wide = Func::new(AddrType::I64, Vec::new(), Vec::new());
        wide.i32_const(1).i32_to_addr();
        assert!(!narrow.finish().into_vec().contains(&op::I64_EXTEND_I32_U));
        assert!(wide.finish().into_vec().contains(&op::I64_EXTEND_I32_U));
    }

    #[test]
    fn locals_follow_parameters_in_one_index_space() {
        let mut func = Func::new(AddrType::I32, vec![ValType::I32, ValType::I64], Vec::new());
        assert_eq!(func.param(0), Some(LocalIdx(0)));
        assert_eq!(func.param(1), Some(LocalIdx(1)));
        assert_eq!(func.param(2), None);
        assert_eq!(func.local(ValType::F64), LocalIdx(2));
        assert_eq!(func.local(ValType::F64), LocalIdx(3));
    }

    #[test]
    fn locals_of_one_type_encode_as_a_single_run() {
        let mut func = Func::new(AddrType::I32, Vec::new(), Vec::new());
        func.local(ValType::I32);
        func.local(ValType::I32);
        func.local(ValType::I64);
        let body = func.finish().into_vec();
        // Skip the size prefix: two runs, `2 x i32` then `1 x i64`.
        assert_eq!(
            &body[1..],
            &[
                0x02,
                0x02,
                ValType::I32.code(),
                0x01,
                ValType::I64.code(),
                op::END
            ]
        );
    }
}
