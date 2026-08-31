//! Windows lifecycle fibers using the platform's TEB-aware fiber API.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::ffi::c_void;

/// Native body of one zero-argument lifecycle function.
pub(crate) type LifecycleEntry = extern "C" fn();

/// Bytes of stack address space Windows reserves for each lifecycle fiber.
///
/// Sized like a host main-thread stack, for the same reason as the Unix
/// arena: a lifecycle runs full application frameworks. `CreateFiberEx`
/// commits pages on demand behind a guard page, so only reached depth is paid.
const STACK_BYTES: usize = 8 * 1024 * 1024;

/// Preserves floating-point state across fiber switches on every architecture.
const FIBER_FLAG_FLOAT_SWITCH: u32 = 0x1;

type FiberStart = unsafe extern "system" fn(*mut c_void);

#[link(name = "kernel32")]
unsafe extern "system" {
    fn ConvertThreadToFiber(parameter: *mut c_void) -> *mut c_void;
    fn ConvertFiberToThread() -> i32;
    fn CreateFiberEx(
        stack_commit_size: usize,
        stack_reserve_size: usize,
        flags: u32,
        start: Option<FiberStart>,
        parameter: *mut c_void,
    ) -> *mut c_void;
    fn DeleteFiber(fiber: *mut c_void);
    fn SwitchToFiber(fiber: *mut c_void);
}

thread_local! {
    static SCHEDULER: RefCell<Scheduler> = const { RefCell::new(Scheduler::new()) };
    static ROOT_FIBER: Cell<*mut c_void> = const { Cell::new(std::ptr::null_mut()) };
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

/// Drops every lifecycle and returns the scheduler fiber to a thread.
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
    if !should_yield {
        return;
    }
    ROOT_FIBER.with(|root| {
        let root = root.get();
        if !root.is_null() {
            // SAFETY: `Scheduler::pump` installs the converted caller fiber
            // for exactly the duration a lifecycle is running.
            unsafe { SwitchToFiber(root) };
        }
    });
}

struct Scheduler {
    root: *mut c_void,
    fibers: VecDeque<Fiber>,
}

impl Scheduler {
    const fn new() -> Self {
        Self {
            root: std::ptr::null_mut(),
            fibers: VecDeque::new(),
        }
    }

    fn start(&mut self, entry: LifecycleEntry) -> Result<(), &'static str> {
        self.ensure_root()?;
        let mut state = Box::new(FiberState {
            entry,
            finished: false,
        });
        let parameter = std::ptr::from_mut(state.as_mut()).cast();
        // SAFETY: `parameter` remains stable in `state` until DeleteFiber, and
        // the callback has the exact Windows fiber ABI. Commit size zero takes
        // the default one-page commit inside the reservation, and
        // FIBER_FLAG_FLOAT_SWITCH preserves floating-point state across
        // switches on every architecture.
        let handle = unsafe {
            CreateFiberEx(
                0,
                STACK_BYTES,
                FIBER_FLAG_FLOAT_SWITCH,
                Some(fiber_bootstrap),
                parameter,
            )
        };
        if handle.is_null() {
            return Err("could not allocate a Windows lifecycle fiber");
        }
        self.fibers.push_back(Fiber { handle, state });
        Ok(())
    }

    fn ensure_root(&mut self) -> Result<(), &'static str> {
        if !self.root.is_null() {
            return Ok(());
        }
        // SAFETY: the Kira main loop owns this OS thread until reset converts
        // it back after every lifecycle has finished.
        self.root = unsafe { ConvertThreadToFiber(std::ptr::null_mut()) };
        if self.root.is_null() {
            return Err("could not convert the Windows main thread to a fiber");
        }
        Ok(())
    }

    fn pump(&mut self, budget: u64) -> bool {
        let turns = self.fibers.len();
        ROOT_FIBER.with(|root| root.set(self.root));
        for _ in 0..turns {
            let Some(fiber) = self.fibers.pop_front() else {
                break;
            };
            BUDGET.with(|slot| slot.set(budget));
            // SAFETY: the handle belongs to this scheduler and remains live
            // until the lifecycle yields back to `root`.
            unsafe { SwitchToFiber(fiber.handle) };
            if fiber.state.finished {
                // SAFETY: the lifecycle has returned and will never resume.
                unsafe { DeleteFiber(fiber.handle) };
            } else {
                self.fibers.push_back(fiber);
            }
        }
        ROOT_FIBER.with(|root| root.set(std::ptr::null_mut()));
        BUDGET.with(|slot| slot.set(0));
        !self.fibers.is_empty()
    }

    fn reset(&mut self) {
        for fiber in self.fibers.drain(..) {
            // SAFETY: every handle was created by this scheduler and none is
            // running while the caller owns the scheduler again.
            unsafe { DeleteFiber(fiber.handle) };
        }
        if !self.root.is_null() {
            // SAFETY: execution is on the converted root fiber and all child
            // fibers have been deleted.
            let _ = unsafe { ConvertFiberToThread() };
            self.root = std::ptr::null_mut();
        }
        ROOT_FIBER.with(|root| root.set(std::ptr::null_mut()));
        BUDGET.with(|slot| slot.set(0));
    }
}

struct Fiber {
    handle: *mut c_void,
    state: Box<FiberState>,
}

struct FiberState {
    entry: LifecycleEntry,
    finished: bool,
}

unsafe extern "system" fn fiber_bootstrap(parameter: *mut c_void) {
    if parameter.is_null() {
        std::process::abort();
    }
    // SAFETY: `Scheduler::start` passed a stable boxed `FiberState` and keeps
    // it alive until this callback has marked it finished and yielded.
    let state = unsafe { &mut *parameter.cast::<FiberState>() };
    (state.entry)();
    state.finished = true;
    loop {
        ROOT_FIBER.with(|root| {
            let root = root.get();
            if root.is_null() {
                std::process::abort();
            }
            // SAFETY: the scheduler root remains live until this finished
            // fiber yields and is deleted.
            unsafe { SwitchToFiber(root) };
        });
    }
}
