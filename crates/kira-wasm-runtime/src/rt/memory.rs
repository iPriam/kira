//! The generated bump allocator and byte copy.
//!
//! Both are emitted into the module rather than imported: a Kira program on the
//! Web must not need a host to hand it an allocator, and a module that carried
//! its own memory but borrowed its allocation would answer to two owners.

use crate::encode::ValType;
use crate::func::{BlockType, FuncIdx, GlobalIdx};
use crate::layout;
use crate::module::Module;

/// Emits `alloc(size) -> address`: the bump allocator.
///
/// Rounds `size` up to [`layout::ALIGN`], hands back the old bump pointer, and
/// grows linear memory when the request runs past its end. A host that refuses
/// to grow is out of memory, which is a trap and not a wrong answer.
pub fn define_alloc(
    module: &mut Module,
    index: FuncIdx,
    heap: GlobalIdx,
    trap_oom: FuncIdx,
) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![addr], vec![addr]);

    let Some(size) = func.param(0) else {
        return false;
    };
    let ptr = func.local(addr);
    let end = func.local(addr);
    let have = func.local(addr);

    // ptr = heap; end = ptr + align_up(size)
    func.global_get(heap).local_tee(ptr);
    func.local_get(size)
        .addr_const(u64::from(layout::ALIGN) - 1)
        .addr_add()
        .addr_const(!(u64::from(layout::ALIGN) - 1))
        .addr_and();
    func.addr_add().local_set(end);

    // have = memory.size * PAGE_BYTES
    func.memory_size()
        .addr_const(u64::from(layout::PAGE_BYTES))
        .addr_mul()
        .local_set(have);

    // if end > have: grow by the pages the shortfall needs, and trap if the
    // host refuses.
    func.local_get(end).local_get(have).addr_gt_u();
    func.if_(BlockType::Empty);
    {
        // pages = ceil((end - have) / PAGE_BYTES)
        func.local_get(end)
            .local_get(have)
            .addr_sub()
            .addr_const(u64::from(layout::PAGE_BYTES) - 1)
            .addr_add()
            .addr_const(u64::from(layout::PAGE_BYTES))
            .addr_div_u();
        func.memory_grow();
        // memory.grow answers with the previous size, or -1 when it refused.
        func.addr_const(u64::MAX).addr_eq();
        func.if_(BlockType::Empty);
        func.call(trap_oom).unreachable();
        func.end();
    }
    func.end();

    func.local_get(end).global_set(heap);
    func.local_get(ptr);

    module.define(index, func)
}

/// Emits `memcpy(destination, source, length)`.
///
/// One `memory.copy`. Copies only ever run over a string, and a string is the
/// length its header says, so there is nothing to decide byte by byte — and the
/// engine's copy is the one place a host can do better than a loop this backend
/// writes.
///
/// This is the module's one use of bulk memory, which is baseline in every
/// engine that has shipped Memory64 and every browser since 2021. A byte loop
/// here made concatenation quadratic in *interpreted work* on top of quadratic
/// in bytes, which a program that builds a string in a loop pays for twice.
///
/// `memory.copy` handles overlap and a zero length itself, so a zero-length
/// string is not a special case anywhere.
pub fn define_memcpy(module: &mut Module, index: FuncIdx) -> bool {
    let addr = module.addr().val();
    let mut func = module.func(vec![addr, addr, ValType::I32], Vec::new());

    let (Some(destination), Some(source), Some(length)) =
        (func.param(0), func.param(1), func.param(2))
    else {
        return false;
    };

    func.local_get(destination);
    func.local_get(source);
    func.local_get(length).i32_to_addr();
    func.memory_copy();

    module.define(index, func)
}
