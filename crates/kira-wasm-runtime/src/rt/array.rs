//! The generated array runtime: allocation, bounds checking, and growth.
//!
//! # An array is a header plus a moving item block
//!
//! ```text
//!  header (the array's value — its address never changes)
//!    +0*A  len     how many elements are live
//!    +1*A  cap     how many the item block has room for
//!    +2*A  items   address of the element storage
//! ```
//!
//! The indirection is the whole design. `xs.append(v)` must be visible through
//! every path that reaches `xs` — a local, a struct field, an element of an
//! outer array — and all of those hold the *header's* address. So the header
//! stays put and the item block is what moves when the array outgrows it.
//! Storing the elements inline would mean growth changed the array's address,
//! and every holder of the old one would be looking at a stale array.
//!
//! # Why growth allocates and copies
//!
//! The heap here never frees (see [`crate::layout`]), so there is no `realloc`
//! to extend a block in place: growing means allocating a bigger one, copying,
//! and abandoning the old. The old block is garbage for the rest of the
//! module's life, which is the same bargain every other allocation here makes —
//! a module is torn down whole when its program ends. Capacity doubles, so a
//! loop of appends copies a total of O(n) elements rather than O(n²).
//!
//! # These helpers are generic over the element type
//!
//! Every one takes the element size as a parameter rather than being emitted
//! once per array type. Element *size* is all they need: the load and store of
//! an actual element are emitted at the call site, where the type is known. So
//! `[Int]`, `[String]`, and `[[Point]]` share one copy of this code, and only
//! the deep-copy helper — which has to recurse — is per-type.

use crate::encode::ValType;
use crate::func::{AddrType, BlockType, FuncIdx};
use crate::module::Module;

/// Byte offset of the `len` field within an array header.
pub const LEN_OFFSET: u32 = 0;

/// The header's size, and the offsets of `cap` and `items`, at an address
/// width.
///
/// Every field is address-wide: a length that could not index memory would be
/// a length no array could have.
#[derive(Debug, Clone, Copy)]
pub struct HeaderLayout {
    /// Byte offset of `cap`.
    pub cap: u64,
    /// Byte offset of `items`.
    pub items: u64,
    /// The header's total size in bytes.
    pub size: u32,
}

impl HeaderLayout {
    /// The header layout for an address width.
    pub fn of(addr: AddrType) -> Self {
        let word = u64::from(word_bytes(addr));
        Self {
            cap: word,
            items: word * 2,
            size: (word * 3) as u32,
        }
    }
}

/// How many bytes one address occupies.
pub fn word_bytes(addr: AddrType) -> u32 {
    match addr.val() {
        ValType::I64 => 8,
        _ => 4,
    }
}

/// Handles to the generated array helpers.
#[derive(Debug, Clone, Copy)]
pub struct Arrays {
    /// `array_new(count, esize) -> header`
    pub new: FuncIdx,
    /// `array_len(header) -> i64`
    pub len: FuncIdx,
    /// `array_items(header) -> address`
    pub items: FuncIdx,
    /// `array_slot(header, index, esize) -> address` — bounds-checked.
    pub slot: FuncIdx,
    /// `array_push_slot(header, esize) -> address` — grows, then makes room.
    pub push_slot: FuncIdx,
    /// Traps: an array index at or past the end.
    pub trap_bounds: FuncIdx,
    /// Traps: a negative array index.
    pub trap_negative: FuncIdx,
}

/// The VM's trap text for an out-of-range index, so a trapping program reads
/// the same on every engine.
///
/// This is what `kira-vm-runtime` renders for `VmError::IndexOutOfBounds`. A
/// copy is a copy, so the parity tests compare the two rather than trusting
/// this comment.
pub const INDEX_OUT_OF_BOUNDS: &str = "array index is out of bounds";

/// The VM's trap text for a negative index.
///
/// Neither engine names the offending index: a trap path cannot format a number
/// into a message without allocating mid-trap, and the VM does not either (see
/// `VmError::NegativeIndex`), so both state only the kind. Naming the index was
/// tried and dropped — it broke parity, caught by a differential test, not
/// inspection. What parity requires is that both **trap**, and that a negative
/// index is a *different* trap from one past the end — which is what these two
/// distinct messages carry.
pub const INDEX_NEGATIVE: &str = "array index is negative";

/// Declares every array helper, reserving indices before any body exists.
pub fn declare(module: &mut Module) -> Arrays {
    let addr = module.addr().val();
    Arrays {
        new: module.declare(vec![addr, addr], vec![addr]),
        len: module.declare(vec![addr], vec![ValType::I64]),
        items: module.declare(vec![addr], vec![addr]),
        slot: module.declare(vec![addr, ValType::I64, addr], vec![addr]),
        push_slot: module.declare(vec![addr, addr], vec![addr]),
        trap_bounds: module.declare(Vec::new(), Vec::new()),
        trap_negative: module.declare(Vec::new(), Vec::new()),
    }
}

/// Emits `array_new(count, esize) -> header`.
///
/// Allocates a header and an item block sized for exactly `count`, and sets
/// `len == cap == count`. A literal's array is full the moment it exists; an
/// empty one gets a zero-capacity block, which the first `append` grows.
pub fn define_new(module: &mut Module, index: FuncIdx, alloc: FuncIdx) -> bool {
    let addr_ty = module.addr();
    let addr = addr_ty.val();
    let header = HeaderLayout::of(addr_ty);
    let mut func = module.func(vec![addr, addr], vec![addr]);

    let (Some(count), Some(esize)) = (func.param(0), func.param(1)) else {
        return false;
    };
    let object = func.local(addr);

    func.addr_const(u64::from(header.size))
        .call(alloc)
        .local_set(object);

    // len = cap = count
    func.local_get(object)
        .local_get(count)
        .addr_store(u64::from(LEN_OFFSET));
    func.local_get(object)
        .local_get(count)
        .addr_store(header.cap);

    // items = alloc(count * esize)
    func.local_get(object);
    func.local_get(count).local_get(esize).addr_mul();
    func.call(alloc);
    func.addr_store(header.items);

    func.local_get(object);
    module.define(index, func)
}

/// Emits `array_len(header) -> i64`.
pub fn define_len(module: &mut Module, index: FuncIdx) -> bool {
    let addr_ty = module.addr();
    let addr = addr_ty.val();
    let mut func = module.func(vec![addr], vec![ValType::I64]);
    let Some(object) = func.param(0) else {
        return false;
    };
    func.local_get(object)
        .addr_load(u64::from(LEN_OFFSET))
        .addr_to_i64();
    module.define(index, func)
}

/// Emits `array_items(header) -> address`.
pub fn define_items(module: &mut Module, index: FuncIdx) -> bool {
    let addr_ty = module.addr();
    let addr = addr_ty.val();
    let header = HeaderLayout::of(addr_ty);
    let mut func = module.func(vec![addr], vec![addr]);
    let Some(object) = func.param(0) else {
        return false;
    };
    func.local_get(object).addr_load(header.items);
    module.define(index, func)
}

/// Emits `array_slot(header, index, esize) -> address`.
///
/// The bounds check, and the only place one lives: every element read and every
/// element write goes through this, so neither can forget it.
///
/// A negative index and one past the end are checked **separately** and trap
/// differently, because they are different mistakes — the VM draws the same
/// line, and parity is what keeps them drawn in the same place.
pub fn define_slot(module: &mut Module, index: FuncIdx, arrays: &Arrays) -> bool {
    let addr_ty = module.addr();
    let addr = addr_ty.val();
    let mut func = module.func(vec![addr, ValType::I64, addr], vec![addr]);

    let (Some(object), Some(at), Some(esize)) = (func.param(0), func.param(1), func.param(2))
    else {
        return false;
    };

    // A negative index is checked first: it is a wrong computation, not a
    // misjudged length, and the two must not collapse into one message.
    func.local_get(at).i64_const(0).i64_lt_s();
    func.if_(BlockType::Empty);
    func.call(arrays.trap_negative).unreachable();
    func.end();

    // at >= len — unsigned on the `i64`, safe because the negative case is
    // already gone.
    func.local_get(at);
    func.local_get(object).addr_load(u64::from(LEN_OFFSET));
    func.addr_to_i64();
    func.i64_ge_u();
    func.if_(BlockType::Empty);
    func.call(arrays.trap_bounds).unreachable();
    func.end();

    // items + at * esize. The truncation to an address is safe only because the
    // bounds check above proved `at` indexes an object that fits in memory.
    func.local_get(object)
        .addr_load(HeaderLayout::of(addr_ty).items);
    func.local_get(at).i64_to_addr().local_get(esize).addr_mul();
    func.addr_add();
    module.define(index, func)
}

/// Emits `array_push_slot(header, esize) -> address`.
///
/// Makes room for one more element and returns where it goes. Growth doubles
/// the capacity (from one, when there is none), so a run of appends copies O(n)
/// elements in total rather than O(n²).
pub fn define_push_slot(
    module: &mut Module,
    index: FuncIdx,
    alloc: FuncIdx,
    memcpy: FuncIdx,
) -> bool {
    let addr_ty = module.addr();
    let addr = addr_ty.val();
    let header = HeaderLayout::of(addr_ty);
    let mut func = module.func(vec![addr, addr], vec![addr]);

    let (Some(object), Some(esize)) = (func.param(0), func.param(1)) else {
        return false;
    };
    let len = func.local(addr);
    let cap = func.local(addr);
    let fresh = func.local(addr);
    let grown = func.local(addr);

    func.local_get(object)
        .addr_load(u64::from(LEN_OFFSET))
        .local_set(len);
    func.local_get(object).addr_load(header.cap).local_set(cap);

    // if len == cap: grow.
    func.local_get(len).local_get(cap).addr_eq();
    func.if_(BlockType::Empty);
    {
        // grown = cap == 0 ? 1 : cap * 2
        func.local_get(cap).addr_const(0).addr_eq();
        func.if_(BlockType::Value(addr));
        func.addr_const(1);
        func.else_();
        func.local_get(cap).addr_const(2).addr_mul();
        func.end();
        func.local_set(grown);

        // fresh = alloc(grown * esize)
        func.local_get(grown)
            .local_get(esize)
            .addr_mul()
            .call(alloc)
            .local_set(fresh);

        // memcpy(fresh, items, len * esize). The byte count is an `i32`, which
        // is the allocator's existing limit rather than a new one: nothing here
        // can copy more than 4GiB in one call.
        func.local_get(fresh);
        func.local_get(object).addr_load(header.items);
        func.local_get(len)
            .local_get(esize)
            .addr_mul()
            .addr_to_i32();
        func.call(memcpy);

        func.local_get(object)
            .local_get(fresh)
            .addr_store(header.items);
        func.local_get(object)
            .local_get(grown)
            .addr_store(header.cap);
    }
    func.end();

    // len += 1
    func.local_get(object);
    func.local_get(len).addr_const(1).addr_add();
    func.addr_store(u64::from(LEN_OFFSET));

    // items + len * esize — the slot the old length pointed at.
    func.local_get(object).addr_load(header.items);
    func.local_get(len).local_get(esize).addr_mul();
    func.addr_add();
    module.define(index, func)
}
