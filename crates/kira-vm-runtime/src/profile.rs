//! The Kira call stack the VM publishes for asynchronous sampling.
//!
//! A sampling profiler runs on another thread and cannot borrow the
//! interpreter's frame stack. The interpreter therefore mirrors its Kira-level
//! call stack into a lock-free structure any thread may read at any moment: one
//! word per live frame, rewritten as frames are entered and left, with the
//! innermost frame's instruction index updated as it advances.
//!
//! Publication is off unless [`set_enabled`] turned it on before a run starts,
//! and the interpreter selects a dispatch loop without the stores when it is
//! off — so an ordinary run pays nothing for this module existing.
//!
//! # Reading a stack that is being written
//!
//! [`ShadowStack`] is a sequence lock. The writer bumps [`ShadowStack::sequence`]
//! to an odd value, publishes, and bumps it to the next even value; a reader
//! that saw the same even sequence before and after its read saw a stack no
//! writer touched in between. The innermost frame's instruction index is stored
//! outside that protocol, because it changes on every instruction and a reader
//! would never observe a stable window otherwise: it is a single atomic word, so
//! a reader always sees one whole (function, instruction) pair.

use std::cell::OnceCell;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering, fence};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// The deepest Kira call stack a sample can carry, in frames.
///
/// A run deeper than this publishes its outermost frames plus the innermost
/// one, and reports the rest as omitted, rather than growing an unbounded
/// structure a sampler would have to read while it changes. The innermost frame
/// is kept whatever the depth, because that is where time is actually spent.
pub const MAX_SHADOW_DEPTH: usize = 512;

/// How many times a reader retries a stack a writer kept changing under it.
///
/// A call-heavy program changes its stack every few hundred nanoseconds, which
/// is the same order as the read, so a handful of attempts is not enough to
/// keep the loss rate near zero.
const SNAPSHOT_ATTEMPTS: usize = 64;

/// One published Kira call-stack entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShadowFrame {
    /// The bytecode function index.
    pub function: u32,
    /// The instruction index this frame is executing.
    pub pc: u32,
}

/// A profile-local identifier for a thread that runs Kira code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ThreadTag(u32);

impl ThreadTag {
    /// A tag with the given ordinal.
    #[must_use]
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    /// The tag's ordinal, assigned in the order threads first ran Kira code.
    #[must_use]
    pub fn index(self) -> u32 {
        self.0
    }
}

/// Whether the interpreter publishes its call stack.
static ENABLED: AtomicBool = AtomicBool::new(false);

/// Whether the interpreter publishes its call stack.
#[must_use]
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// Turns publication on or off for every run that starts afterwards.
///
/// A run already in flight keeps the dispatch loop it selected when it started,
/// which is what makes the choice free for runs that are not being profiled.
pub fn set_enabled(publish: bool) {
    ENABLED.store(publish, Ordering::Relaxed);
}

/// One thread's published Kira call stack.
#[derive(Debug)]
pub struct ShadowStack {
    tag: ThreadTag,
    name: String,
    /// Even between publications, odd while one is in progress.
    sequence: AtomicU64,
    /// Number of published frames.
    depth: AtomicU32,
    /// `function << 32 | pc`, one per live frame, outermost first.
    entries: Box<[AtomicU64]>,
}

impl ShadowStack {
    fn new(tag: ThreadTag, name: String) -> Self {
        let entries = (0..MAX_SHADOW_DEPTH)
            .map(|_| AtomicU64::new(0))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            tag,
            name,
            sequence: AtomicU64::new(0),
            depth: AtomicU32::new(0),
            entries,
        }
    }

    /// The tag identifying the thread this stack belongs to.
    #[must_use]
    pub fn tag(&self) -> ThreadTag {
        self.tag
    }

    /// The thread's name, as a report shows it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The number of frames currently published.
    #[must_use]
    pub fn depth(&self) -> u32 {
        self.depth.load(Ordering::Relaxed)
    }

    /// Publishes `function` as the frame at `index`, making the stack
    /// `index + 1` frames deep.
    ///
    /// Called when the interpreter's frame depth changes in either direction:
    /// entering rewrites the new innermost frame, and leaving shortens the
    /// stack back onto a frame that is already correct.
    #[inline]
    pub fn enter(&self, index: u32, function: u32) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        self.sequence
            .store(sequence.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        if let Some(slot) = self.slot(index) {
            slot.store(entry_word(function, 0), Ordering::Relaxed);
        }
        self.depth.store(index.saturating_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::Relaxed);
    }

    /// Records the instruction the frame at `index` is executing.
    ///
    /// Outside the sequence lock on purpose: this runs once per interpreted
    /// instruction, and bumping the sequence that often would leave a reader
    /// with no stable window to read the rest of the stack in.
    #[inline]
    pub fn mark(&self, index: u32, function: u32, pc: u32) {
        if let Some(slot) = self.slot(index) {
            slot.store(entry_word(function, pc), Ordering::Relaxed);
        }
    }

    /// The word frame `index` publishes into.
    ///
    /// A frame past the array shares the last slot with the frames above it, so
    /// the innermost frame is always the last one a reader sees however deep
    /// the run went.
    #[inline]
    fn slot(&self, index: u32) -> Option<&AtomicU64> {
        let last = self.entries.len().checked_sub(1)?;
        self.entries.get((index as usize).min(last))
    }

    /// Shortens the stack back to `depth` frames.
    #[inline]
    pub fn leave(&self, depth: u32) {
        let sequence = self.sequence.load(Ordering::Relaxed);
        self.sequence
            .store(sequence.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        self.depth.store(depth, Ordering::Relaxed);
        fence(Ordering::Release);
        self.sequence
            .store(sequence.wrapping_add(2), Ordering::Relaxed);
    }

    /// Reads the stack into `out`, outermost frame first.
    ///
    /// The last frame read is always the innermost one; the count returned is
    /// how many frames sit between it and the frame before it, which a report
    /// shows as an elision.
    ///
    /// Returns `None` when the writer changed the stack under every attempt — a
    /// sampler counts that as a missed sample rather than reporting a stack
    /// that never existed.
    pub fn snapshot(&self, out: &mut Vec<ShadowFrame>) -> Option<u32> {
        for _ in 0..SNAPSHOT_ATTEMPTS {
            let before = self.sequence.load(Ordering::Relaxed);
            if !before.is_multiple_of(2) {
                std::hint::spin_loop();
                continue;
            }
            fence(Ordering::Acquire);
            let depth = self.depth.load(Ordering::Relaxed) as usize;
            let read = depth.min(MAX_SHADOW_DEPTH);
            out.clear();
            out.extend(self.entries[..read].iter().map(|slot| {
                let word = slot.load(Ordering::Relaxed);
                ShadowFrame {
                    function: (word >> 32) as u32,
                    pc: word as u32,
                }
            }));
            fence(Ordering::Acquire);
            if self.sequence.load(Ordering::Relaxed) == before {
                return Some((depth - read) as u32);
            }
        }
        out.clear();
        None
    }
}

/// Packs a frame's function index and instruction index into one word.
#[inline]
const fn entry_word(function: u32, pc: u32) -> u64 {
    ((function as u64) << 32) | pc as u64
}

/// The published stack of one run, restored to its starting depth on drop.
///
/// A native half calling back into the VM starts a second run on the same
/// thread while the first is still live. Its frames belong on top of the ones
/// already published, so a scope records the depth it opened at and every index
/// it publishes is relative to that.
#[derive(Debug)]
pub struct ShadowScope {
    stack: Arc<ShadowStack>,
    base: u32,
}

impl ShadowScope {
    /// Opens a scope on this thread's published stack.
    #[must_use]
    pub fn open() -> Self {
        let stack = thread_stack();
        let base = stack.depth();
        Self { stack, base }
    }

    /// The depth this scope opened at; frame `n` of the run publishes at
    /// `base + n`.
    #[must_use]
    pub fn base(&self) -> u32 {
        self.base
    }

    /// The stack being published to.
    #[must_use]
    pub fn stack(&self) -> &ShadowStack {
        &self.stack
    }
}

impl Drop for ShadowScope {
    fn drop(&mut self) {
        self.stack.leave(self.base);
    }
}

/// Every published stack, and the tag counter that names the next one.
#[derive(Debug, Default)]
struct Registry {
    stacks: Vec<Weak<ShadowStack>>,
    next_tag: u32,
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

thread_local! {
    static THREAD_STACK: OnceCell<Arc<ShadowStack>> = const { OnceCell::new() };
}

/// The registry, recovering a lock poisoned by a panicking sampler.
///
/// A poisoned registry still describes exactly the same live stacks; refusing
/// to profile because a reader panicked once would lose the profile that would
/// explain it.
fn registry() -> std::sync::MutexGuard<'static, Registry> {
    let lock = REGISTRY.get_or_init(|| Mutex::new(Registry::default()));
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// This thread's published stack, creating and registering it on first use.
#[must_use]
pub fn thread_stack() -> Arc<ShadowStack> {
    THREAD_STACK.with(|cell| Arc::clone(cell.get_or_init(register_thread_stack)))
}

fn register_thread_stack() -> Arc<ShadowStack> {
    let mut registry = registry();
    let tag = ThreadTag(registry.next_tag);
    registry.next_tag = registry.next_tag.saturating_add(1);
    let current = std::thread::current();
    let name = match current.name() {
        Some(name) => name.to_owned(),
        None => format!("thread-{}", tag.index()),
    };
    let stack = Arc::new(ShadowStack::new(tag, name));
    registry.stacks.push(Arc::downgrade(&stack));
    stack
}

/// Every stack whose thread is still alive, in registration order.
///
/// Threads that have finished are dropped from the registry as they are found,
/// so a long session that starts and finishes many threads does not accumulate
/// dead entries.
#[must_use]
pub fn live_stacks() -> Vec<Arc<ShadowStack>> {
    let mut registry = registry();
    let mut live = Vec::with_capacity(registry.stacks.len());
    registry.stacks.retain(|weak| match weak.upgrade() {
        Some(stack) => {
            live.push(stack);
            true
        }
        None => false,
    });
    live
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_snapshot_reads_every_published_frame_innermost_last() {
        let stack = ShadowStack::new(ThreadTag(0), "test".to_owned());
        stack.enter(0, 7);
        stack.mark(0, 7, 3);
        stack.enter(1, 9);
        stack.mark(1, 9, 11);

        let mut frames = Vec::new();
        assert_eq!(stack.snapshot(&mut frames), Some(0));
        assert_eq!(
            frames,
            vec![
                ShadowFrame { function: 7, pc: 3 },
                ShadowFrame {
                    function: 9,
                    pc: 11
                },
            ]
        );
    }

    #[test]
    fn leaving_shortens_the_stack_without_disturbing_the_frames_below() {
        let stack = ShadowStack::new(ThreadTag(0), "test".to_owned());
        stack.enter(0, 1);
        stack.mark(0, 1, 4);
        stack.enter(1, 2);
        stack.leave(1);

        let mut frames = Vec::new();
        assert_eq!(stack.snapshot(&mut frames), Some(0));
        assert_eq!(frames, vec![ShadowFrame { function: 1, pc: 4 }]);
    }

    #[test]
    fn a_stack_deeper_than_the_array_keeps_its_innermost_frame() {
        let stack = ShadowStack::new(ThreadTag(0), "test".to_owned());
        let deepest = MAX_SHADOW_DEPTH as u32 + 2;
        for index in 0..=deepest {
            stack.enter(index, index);
        }
        stack.mark(deepest, deepest, 5);

        let mut frames = Vec::new();
        assert_eq!(stack.snapshot(&mut frames), Some(3));
        assert_eq!(frames.len(), MAX_SHADOW_DEPTH);
        assert_eq!(
            frames.last().copied(),
            Some(ShadowFrame {
                function: deepest,
                pc: 5
            })
        );
    }

    #[test]
    fn a_scope_restores_the_depth_it_opened_at() {
        let outer = ShadowScope::open();
        outer.stack().enter(outer.base(), 1);
        let published = outer.stack().depth();
        {
            let inner = ShadowScope::open();
            assert_eq!(inner.base(), published);
            inner.stack().enter(inner.base(), 2);
            assert_eq!(inner.stack().depth(), published + 1);
        }
        assert_eq!(outer.stack().depth(), published);
    }

    #[test]
    fn every_thread_that_runs_kira_code_gets_its_own_stack() {
        let first = thread_stack();
        let second = std::thread::spawn(|| thread_stack().tag().index())
            .join()
            .expect("the sampled thread finished");
        assert_ne!(first.tag().index(), second);
        assert!(live_stacks().iter().any(|stack| Arc::ptr_eq(stack, &first)));
    }
}
