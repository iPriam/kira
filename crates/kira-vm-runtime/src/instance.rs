//! A loaded library with a heap that outlives one call.
//!
//! [`Program::call`](crate::Program::call) runs each call on a fresh
//! [`Heap`] and drops it at the end, which is right for the hybrid seam — the
//! native half asks a question and gets an answer — and wrong for a library. A
//! library is asked for a `Button` and then asked about that same `Button`
//! later, and Kira has no globals, so there is nowhere for the object to live
//! between the two calls. [`Instance`] is that somewhere.
//!
//! # Roots are the whole idea
//!
//! An instance owns one heap for its whole life, plus a **root table**: the set
//! of values the consumer still holds a name for. A call that produces an object
//! roots it and hands back a [`RootId`]; [`Instance::release`] un-roots it and
//! frees what it owned. Everything else the call allocated is freed by the call,
//! exactly as it always was.
//!
//! Root ids are minted from a counter and **never reused**. That is the one
//! property that makes a released handle a *typed error*
//! ([`VmError::DanglingRoot`]) rather than a silent hit on whatever object later
//! took that heap slot — a wrong answer about which object is a use-after-free,
//! and this is the design that makes one unrepresentable.
//!
//! # What a handle argument does
//!
//! It is **copied in**, not lent. A class is a value type in Kira — passing one
//! to a function copies it, and the export boundary refuses `move` and
//! `borrow mut` on a parameter for exactly that reason — so a copy is what the
//! language already means, and it keeps the affine drop discipline intact: the
//! callee owns its copy and frees it at return, while the root keeps owning the
//! original. Mutation inside the call is not visible to the consumer afterwards,
//! which is the same answer Kira gives a Kira caller.
//!
//! # What "balanced" means now
//!
//! When a heap belongs to one run, balanced means `current == 0` at exit. A heap
//! that outlives a call cannot say that between calls — a live root is *supposed*
//! to hold storage. So the invariant moves out one level:
//!
//! > **Between calls, everything live in the heap is owned by a live root.**
//!
//! which is checkable, and [`Instance::finish`] is where it is checked: it
//! releases every remaining root and returns the heap's accounting, and
//! `current` is 0 for an instance that balanced. An instance that never roots
//! anything reduces to the old rule — `finish` reports 0 after every call —
//! and a call that *traps* still balances, because a trap unwinds its frames and
//! operand stack into the heap it borrowed rather than abandoning them.
//!
//! That holds for **every** trap a validating module can reach, not only the
//! ones a Kira compiler emits. [`Module::validate`](kira_bytecode::Module) proves
//! structure, not stack typing, so an ill-typed `.kbc` from anywhere can trap
//! with a heap value already popped into a local the unwind cannot see — which
//! is why each such path frees what it holds before it returns. When the heap
//! died with the call that stranding was invisible; here it would be a permanent
//! leak that [`finish`](Instance::finish) reports forever, and later steps read
//! `finish().current == 0` as proof of balance.

use std::collections::BTreeMap;

use kira_bytecode::module::Module;
use kira_runtime_abi::{HostCapabilities, NativeArg, NativeResult};

use crate::error::VmError;
use crate::interp::{Program, Vm, VmScratch, check_signature};
use crate::value::{Heap, HeapStats, Value};

/// The name a consumer holds for one object living in an [`Instance`].
///
/// Opaque on purpose: it is a ticket into the instance's root table, not an
/// address, an index, or anything the holder may compute with. It crosses the
/// seam as the word of a [`NativeArg::Handle`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RootId(u64);

impl RootId {
    /// The word this root crosses the boundary as.
    pub fn as_word(self) -> u64 {
        self.0
    }

    /// Reads a root back from the word a consumer presented.
    ///
    /// Says nothing about whether it is live — only [`Instance`] knows that, and
    /// it answers with a typed error.
    pub fn from_word(word: u64) -> RootId {
        RootId(word)
    }
}

/// A library loaded onto a heap that survives between calls.
///
/// Single-threaded by construction (`&mut self` on every call), matching the
/// wrapper types the consumer holds.
pub struct Instance {
    program: Program,
    heap: Heap,
    /// The values the consumer still holds a name for.
    ///
    /// A `BTreeMap` rather than a hash map: root ids are minted in order, the
    /// table is small, and an ordered map keeps `release_all` deterministic
    /// without depending on a hasher the portable core would have to seed.
    roots: BTreeMap<RootId, Value>,
    /// The next root id to mint. Only ever increases.
    next_root: u64,
    /// Reusable interpreter storage returned after each call. Task state is
    /// deliberately kept inside each VM run, so task handles never cross this
    /// library boundary.
    scratch: VmScratch,
}

impl Instance {
    /// Loads `program` onto a fresh persistent heap.
    pub fn new(program: Program) -> Instance {
        Instance {
            program,
            heap: Heap::new(),
            // Zero is never minted, so a zeroed word is never a live handle.
            next_root: 1,
            roots: BTreeMap::new(),
            scratch: VmScratch::default(),
        }
    }

    /// Validates `module` and loads it, or reports why it cannot be run.
    pub fn load(module: Module) -> Result<Instance, VmError> {
        Ok(Instance::new(Program::load(module)?))
    }

    /// The program this instance runs.
    pub fn program(&self) -> &Program {
        &self.program
    }

    /// How many objects the consumer still holds a name for.
    pub fn live_roots(&self) -> usize {
        self.roots.len()
    }

    /// Heap accounting as it stands right now.
    ///
    /// `current` counts everything live, which between calls is exactly what the
    /// live roots own. [`Instance::finish`] is where that becomes a zero.
    pub fn stats(&self) -> HeapStats {
        self.heap.stats()
    }

    /// Calls one exported function and returns what it produced.
    ///
    /// Speaks the same seam vocabulary as [`Program::call`], with one addition
    /// this heap is what makes possible: a [`NativeArg::Handle`] argument
    /// resolves to the object its root names, and a result that is an object is
    /// rooted here and handed back as [`NativeResult::Handle`].
    ///
    /// Ownership: **arguments borrow** (a `&str` is copied in; a handle's object
    /// is copied in and the root keeps the original), and **the result owns** (a
    /// returned string is an owned `String`; a returned object is a root the
    /// caller must [`release`](Instance::release)).
    pub fn call(
        &mut self,
        host: &mut dyn HostCapabilities,
        function_id: u32,
        args: &[NativeArg<'_>],
    ) -> Result<NativeResult, VmError> {
        check_signature(self.program.module(), function_id, args.len())?;

        let lowered = self.lower_args(function_id, args)?;

        // The VM runs on this instance's heap and gives it back, whether the
        // call returned or trapped — so a trap's reclamation lands here too.
        let scratch = std::mem::take(&mut self.scratch);
        let mut vm = Vm::new_with_scratch(host, std::mem::take(&mut self.heap), scratch);
        let outcome = vm.enter_values(self.program.module(), function_id, lowered);
        let (heap, scratch) = vm.into_heap_and_scratch();
        self.heap = heap;
        self.scratch = scratch;

        self.lift_result(function_id, outcome?)
    }

    /// Brings the caller's arguments into this heap, resolving handles.
    ///
    /// On any refusal every argument already lowered is freed, so a rejected
    /// call leaves the heap exactly as it found it.
    fn lower_args(&mut self, function: u32, args: &[NativeArg<'_>]) -> Result<Vec<Value>, VmError> {
        let mut lowered: Vec<Value> = Vec::with_capacity(args.len());
        for argument in args {
            let value = match *argument {
                NativeArg::Handle(word) => match self.roots.get(&RootId(word)).copied() {
                    // Copied, not lent: see the module docs. The copy is the
                    // callee's to free, and the root keeps the original.
                    Some(rooted) => Ok(self.heap.copy_value(rooted)),
                    None => Err(VmError::DanglingRoot { root: word }),
                },
                // Every other arm of `lower` produces a value, so the refusal
                // below is unreachable — it is here because a library never gets
                // to end its caller's process over a case it thinks impossible.
                other => self.heap.lower(other).ok_or(VmError::UncrossableExport {
                    function,
                    kind: "this argument",
                }),
            };
            match value {
                Ok(value) => lowered.push(value),
                Err(error) => {
                    self.discard(lowered);
                    return Err(error);
                }
            }
        }
        Ok(lowered)
    }

    /// Frees a batch of values this heap owns.
    fn discard(&mut self, values: impl IntoIterator<Item = Value>) {
        for value in values {
            self.heap.drop_value(value);
        }
    }

    /// Turns what the call produced into what crosses back.
    ///
    /// An object is rooted rather than copied out: it has no crossing form other
    /// than a name, and this heap is where it stays.
    fn lift_result(&mut self, function: u32, result: Value) -> Result<NativeResult, VmError> {
        match result {
            Value::Struct(_) => match self.mint_root() {
                Ok(root) => {
                    self.roots.insert(root, result);
                    Ok(NativeResult::Handle(root.as_word()))
                }
                Err(error) => {
                    self.heap.drop_value(result);
                    Err(error)
                }
            },
            // Refused rather than substituted, for the reason every seam refusal
            // here gives: the frontend rejects these on an `@Export` signature,
            // so a value of one arriving is a disagreement worth naming.
            Value::Array(_) | Value::Enum(_) => {
                let kind = if matches!(result, Value::Array(_)) {
                    "an array result"
                } else {
                    "an enum result"
                };
                self.heap.drop_value(result);
                Err(VmError::UncrossableExport { function, kind })
            }
            scalar => {
                let lifted = self.heap.lift(scalar);
                self.heap.drop_value(scalar);
                lifted.ok_or(VmError::UncrossableExport {
                    function,
                    kind: "this result",
                })
            }
        }
    }

    /// Mints the next never-before-used root id.
    fn mint_root(&mut self) -> Result<RootId, VmError> {
        let id = self.next_root;
        self.next_root = id.checked_add(1).ok_or(VmError::RootSpaceExhausted)?;
        Ok(RootId(id))
    }

    /// Releases a root, freeing the object it named.
    ///
    /// A root that is not live is [`VmError::DanglingRoot`], which covers both
    /// releasing twice and presenting another instance's handle.
    pub fn release(&mut self, root: RootId) -> Result<(), VmError> {
        let value = self
            .roots
            .remove(&root)
            .ok_or(VmError::DanglingRoot { root: root.0 })?;
        self.heap.drop_value(value);
        Ok(())
    }

    /// Releases every root still live, freeing what they named.
    pub fn release_all(&mut self) {
        let rooted = std::mem::take(&mut self.roots);
        self.discard(rooted.into_values());
    }

    /// Releases everything and reports the heap's final accounting.
    ///
    /// This is the instance's balance point: `current` is 0 for an instance
    /// whose every allocation was reclaimed. Consuming `self` is what makes the
    /// number mean something — nothing can allocate after it.
    pub fn finish(mut self) -> HeapStats {
        self.release_all();
        self.heap.stats()
    }
}

#[cfg(test)]
#[path = "instance_tests.rs"]
mod tests;
