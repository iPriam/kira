//! Cooperative native fibers for main-thread lifecycles on Unix hosts.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;
use std::ptr::NonNull;

/// Native body of one zero-argument lifecycle function.
pub(crate) type LifecycleEntry = extern "C" fn();

/// Bytes of virtual address space reserved for each lifecycle stack.
///
/// Sized like a host main-thread stack: a lifecycle owns the process main
/// thread and runs full application frameworks (a UI toolkit lowering a deep
/// widget tree overflows a small stack). Pages commit on first touch, so an
/// instance pays only for the depth it actually reaches.
const STACK_BYTES: usize = 8 * 1024 * 1024;
/// Maximum simultaneously live lifecycle instances on one main thread.
const STACK_COUNT: usize = 256;

thread_local! {
    static SCHEDULER: RefCell<Scheduler> = const { RefCell::new(Scheduler::new()) };
    static CURRENT_FIBER: Cell<*mut Context> = const { Cell::new(std::ptr::null_mut()) };
    static SCHEDULER_CONTEXT: Cell<*mut Context> = const { Cell::new(std::ptr::null_mut()) };
    static CURRENT_FINISHED: Cell<*mut bool> = const { Cell::new(std::ptr::null_mut()) };
    static CURRENT_ENTRY: Cell<Option<LifecycleEntry>> = const { Cell::new(None) };
    static BUDGET: Cell<u64> = const { Cell::new(0) };
}

/// Adds one lifecycle instance to this thread's scheduler.
pub(crate) fn start(entry: LifecycleEntry) -> Result<(), &'static str> {
    SCHEDULER.with_borrow_mut(|scheduler| scheduler.start(entry))
}

/// Runs one slice of every active lifecycle, returning whether any remain.
pub(crate) fn pump(budget: u64) -> bool {
    SCHEDULER.with_borrow_mut(|scheduler| scheduler.pump(budget))
}

/// Returns whether this thread owns unfinished lifecycle fibers.
pub(crate) fn active() -> bool {
    SCHEDULER.with_borrow(|scheduler| !scheduler.fibers.is_empty())
}

/// Drops every lifecycle and releases its stack at a process-run boundary.
pub(crate) fn reset() {
    SCHEDULER.with_borrow_mut(Scheduler::reset);
}

/// Cooperative checkpoint emitted into lifecycle call trees.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_main_thread_lifecycle_checkpoint() {
    let should_yield = BUDGET.with(|budget| {
        let remaining = budget.get();
        if remaining == 0 {
            true
        } else {
            budget.set(remaining - 1);
            false
        }
    });
    if should_yield {
        yield_to_scheduler();
    }
}

/// One scheduler and its contiguous virtual stack arena.
struct Scheduler {
    fibers: VecDeque<Fiber>,
    stacks: Option<StackPool>,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            fibers: VecDeque::new(),
            stacks: None,
        }
    }

    fn start(&mut self, entry: LifecycleEntry) -> Result<(), &'static str> {
        if self.stacks.is_none() {
            self.stacks = Some(StackPool::reserve()?);
        }
        let stack = self
            .stacks
            .as_mut()
            .and_then(StackPool::allocate)
            .ok_or("main-thread lifecycle stack capacity is exhausted")?;
        self.fibers.push_back(Fiber::new(entry, stack));
        Ok(())
    }

    fn pump(&mut self, budget: u64) -> bool {
        let turns = self.fibers.len();
        for _ in 0..turns {
            let Some(mut fiber) = self.fibers.pop_front() else {
                break;
            };
            let mut scheduler = Context::zeroed();
            CURRENT_FIBER.with(|slot| slot.set(&mut fiber.context));
            SCHEDULER_CONTEXT.with(|slot| slot.set(&mut scheduler));
            CURRENT_FINISHED.with(|slot| slot.set(&mut fiber.finished));
            CURRENT_ENTRY.with(|slot| slot.set(Some(fiber.entry)));
            BUDGET.with(|slot| slot.set(budget));
            // SAFETY: both contexts are live until the fiber yields back. The
            // fiber stack came from its dedicated writable arena slot.
            unsafe { switch_context(&mut scheduler, &fiber.context) };
            clear_current();
            if fiber.finished {
                if let Some(stacks) = self.stacks.as_mut() {
                    stacks.release(fiber.stack);
                }
            } else {
                self.fibers.push_back(fiber);
            }
        }
        !self.fibers.is_empty()
    }

    fn reset(&mut self) {
        self.fibers.clear();
        self.stacks = None;
        clear_current();
    }
}

fn clear_current() {
    CURRENT_FIBER.with(|slot| slot.set(std::ptr::null_mut()));
    SCHEDULER_CONTEXT.with(|slot| slot.set(std::ptr::null_mut()));
    CURRENT_FINISHED.with(|slot| slot.set(std::ptr::null_mut()));
    CURRENT_ENTRY.with(|slot| slot.set(None));
    BUDGET.with(|slot| slot.set(0));
}

/// One suspended lifecycle and its stack-pool slot.
struct Fiber {
    context: Context,
    entry: LifecycleEntry,
    finished: bool,
    stack: StackSlot,
}

impl Fiber {
    fn new(entry: LifecycleEntry, stack: StackSlot) -> Self {
        Self {
            context: Context::initial(stack.top()),
            entry,
            finished: false,
            stack,
        }
    }
}

extern "C" fn fiber_bootstrap() -> ! {
    let Some(entry) = CURRENT_ENTRY.with(Cell::get) else {
        std::process::abort();
    };
    entry();
    CURRENT_FINISHED.with(|slot| {
        let pointer = slot.get();
        if pointer.is_null() {
            std::process::abort();
        }
        // SAFETY: the scheduler points this at the current fiber's stable bool
        // for exactly the duration of the switch.
        unsafe { *pointer = true };
    });
    loop {
        yield_to_scheduler();
    }
}

fn yield_to_scheduler() {
    let from = CURRENT_FIBER.with(Cell::get);
    let to = SCHEDULER_CONTEXT.with(Cell::get);
    if from.is_null() || to.is_null() {
        return;
    }
    // SAFETY: the two TLS slots are installed only around `Scheduler::pump`'s
    // switch and remain live until this switch returns there.
    unsafe { switch_context(from, to) };
}

/// A contiguous virtual-memory arena with one guard page between stacks.
struct StackPool {
    base: NonNull<c_void>,
    page: usize,
    stride: usize,
    free: Vec<usize>,
}

impl StackPool {
    fn reserve() -> Result<Self, &'static str> {
        // SAFETY: sysconf has no memory contract and returns the host page size.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        let page = usize::try_from(page).map_err(|_| "could not read the host page size")?;
        let stack = STACK_BYTES.div_ceil(page) * page;
        let stride = stack
            .checked_add(page)
            .ok_or("main-thread lifecycle stack arena is too large")?;
        let length = stride
            .checked_mul(STACK_COUNT)
            .ok_or("main-thread lifecycle stack arena is too large")?;
        // SAFETY: this reserves inaccessible anonymous virtual address space;
        // individual stack spans are made writable by `allocate`.
        let pointer = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            )
        };
        if pointer == libc::MAP_FAILED {
            return Err("could not reserve main-thread lifecycle stacks");
        }
        let Some(base) = NonNull::new(pointer) else {
            return Err("the lifecycle stack arena mapped at a null address");
        };
        Ok(Self {
            base,
            page,
            stride,
            free: (0..STACK_COUNT).rev().collect(),
        })
    }

    fn allocate(&mut self) -> Option<StackSlot> {
        let index = self.free.pop()?;
        // SAFETY: `index` names one stride in the reserved mapping. Its first
        // page remains inaccessible as the guard; the rest becomes the stack.
        let bottom = unsafe {
            self.base
                .as_ptr()
                .cast::<u8>()
                .add(index * self.stride + self.page)
        };
        let length = self.stride - self.page;
        // SAFETY: the span is page-aligned and wholly inside the mapping.
        if unsafe { libc::mprotect(bottom.cast(), length, libc::PROT_READ | libc::PROT_WRITE) } != 0
        {
            self.free.push(index);
            return None;
        }
        Some(StackSlot {
            index,
            bottom,
            length,
        })
    }

    fn release(&mut self, slot: StackSlot) {
        // SAFETY: this is the exact span made writable by `allocate`, and the
        // fiber has finished so no saved context can touch it again.
        let _ = unsafe { libc::mprotect(slot.bottom.cast(), slot.length, libc::PROT_NONE) };
        self.free.push(slot.index);
    }
}

impl Drop for StackPool {
    fn drop(&mut self) {
        let length = self.stride * STACK_COUNT;
        // A lifecycle may end the process with `exit`, which runs this TLS
        // destructor while execution still occupies a slot in this arena.
        // Unmapping would pull the stack out from under the running thread,
        // so the mapping is left for process teardown to reclaim.
        let marker = 0u8;
        let stack_pointer = std::ptr::from_ref(&marker) as usize;
        let base = self.base.as_ptr() as usize;
        if (base..base.saturating_add(length)).contains(&stack_pointer) {
            return;
        }
        // SAFETY: `base..base+length` is the mapping created by `reserve`, no
        // fiber survives the scheduler that owns this pool, and the check
        // above proves the current stack lies outside it.
        let _ = unsafe { libc::munmap(self.base.as_ptr(), length) };
    }
}

struct StackSlot {
    index: usize,
    bottom: *mut u8,
    length: usize,
}

impl StackSlot {
    fn top(&self) -> *mut u8 {
        // SAFETY: `bottom..bottom+length` is this slot's writable stack span.
        unsafe { self.bottom.add(self.length) }
    }
}

#[cfg(target_arch = "aarch64")]
#[repr(C, align(16))]
struct Context {
    words: [usize; 13],
}

#[cfg(target_arch = "aarch64")]
impl Context {
    const fn zeroed() -> Self {
        Self { words: [0; 13] }
    }

    fn initial(stack_top: *mut u8) -> Self {
        let mut context = Self::zeroed();
        context.words[0] = stack_top as usize;
        context.words[12] = fiber_bootstrap as *const () as usize;
        context
    }
}

#[cfg(target_arch = "aarch64")]
#[unsafe(naked)]
unsafe extern "C" fn switch_context(from: *mut Context, to: *const Context) {
    core::arch::naked_asm!(
        "mov x9, sp",
        "str x9, [x0, #0]",
        "stp x19, x20, [x0, #8]",
        "stp x21, x22, [x0, #24]",
        "stp x23, x24, [x0, #40]",
        "stp x25, x26, [x0, #56]",
        "stp x27, x28, [x0, #72]",
        "stp x29, x30, [x0, #88]",
        "ldr x9, [x1, #0]",
        "ldp x19, x20, [x1, #8]",
        "ldp x21, x22, [x1, #24]",
        "ldp x23, x24, [x1, #40]",
        "ldp x25, x26, [x1, #56]",
        "ldp x27, x28, [x1, #72]",
        "ldp x29, x30, [x1, #88]",
        "mov sp, x9",
        "ret",
    );
}

#[cfg(target_arch = "x86_64")]
#[repr(C, align(16))]
struct Context {
    words: [usize; 9],
}

#[cfg(target_arch = "x86_64")]
impl Context {
    const fn zeroed() -> Self {
        Self { words: [0; 9] }
    }

    fn initial(stack_top: *mut u8) -> Self {
        let mut context = Self::zeroed();
        // SysV and Win64 both enter a function with RSP eight bytes off the
        // 16-byte call-site alignment. Leave one unused word above the seeded
        // return address so the synthetic `ret` establishes that shape.
        let pointer = unsafe { stack_top.sub(16).cast::<usize>() };
        // SAFETY: the stack slot reserves at least sixteen writable bytes.
        unsafe { pointer.write(fiber_bootstrap as *const () as usize) };
        context.words[0] = pointer as usize;
        context
    }
}

#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn switch_context(from: *mut Context, to: *const Context) {
    core::arch::naked_asm!(
        "mov [rdi + 0], rsp",
        "mov [rdi + 8], rbx",
        "mov [rdi + 16], rbp",
        "mov [rdi + 24], r12",
        "mov [rdi + 32], r13",
        "mov [rdi + 40], r14",
        "mov [rdi + 48], r15",
        "mov [rdi + 56], rdi",
        "mov [rdi + 64], rsi",
        "mov rsp, [rsi + 0]",
        "mov rbx, [rsi + 8]",
        "mov rbp, [rsi + 16]",
        "mov r12, [rsi + 24]",
        "mov r13, [rsi + 32]",
        "mov r14, [rsi + 40]",
        "mov r15, [rsi + 48]",
        "mov rdi, [rsi + 56]",
        "mov rsi, [rsi + 64]",
        "ret",
    );
}
