//! Runtime values and the object heap with affine drop accounting.
//!
//! Scalars (`Int`, `Float`, `Bool`, `Void`) are `Copy` and live inline in a
//! [`Value`]. Strings and structs live on a [`Heap`]; a [`Value::Str`] or
//! [`Value::Struct`] is a handle into it. Both follow value semantics with
//! affine drops: reading a local *copies* it (a fresh allocation, deep for a
//! struct) and every instruction that consumes one *frees* it, so a well-formed
//! run ends with [`HeapStats::current`] at zero.
//!
//! A struct is a plain tuple of values: the VM is structurally typed, so it
//! never learns a struct's name or its field names. The compiler resolved those
//! to indices, which is what lets the same heap serve both kinds of object.
//!
//! # Arrays copy when they are written, not when they are read
//!
//! An array's elements are the one thing here that is *shared*: copying an
//! array takes a new slot pointing at the same elements, and a write through
//! either one gives the writer elements of its own first
//! ([`Heap::make_array_unique`]). Nothing observable changes — the two arrays
//! behave exactly as two deep copies — but reading an array stops costing the
//! whole array, which is what an interpreted UI frame is mostly made of. The
//! native runtime shares an array's item block on the same terms; this is one
//! design serving both engines rather than two.

use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use kira_runtime_abi::{NativeStateToken, NativeStateTypeId, NativeStateValue};

/// A handle to a heap-allocated string.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StrId(u32);

impl StrId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-allocated struct value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StructId(u32);

impl StructId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-allocated array value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArrayId(u32);

impl ArrayId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-allocated enum value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnumId(u32);

impl EnumId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-allocated capture cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellId(u32);

impl CellId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-allocated erased (`Any`) value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ErasedId(u32);

impl ErasedId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-held read of callback state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotId(u32);

impl SnapshotId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A handle to a heap-owned block of C storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CBlockId(u32);

impl CBlockId {
    /// Returns the heap-slot word used by the VM debugger's value view.
    pub(crate) const fn debug_word(self) -> u64 {
        self.0 as u64
    }
}

/// A runtime value on the operand stack or in a local slot.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// A handle to a heap string.
    Str(StrId),
    /// A handle to a heap struct.
    Struct(StructId),
    /// A handle to a heap array.
    Array(ArrayId),
    /// A handle to a heap enum value.
    Enum(EnumId),
    /// A handle to an erased value: what a value becomes on crossing into
    /// `Any`, carrying the type it had on the way in.
    Erased(ErasedId),
    /// A handle to a capture cell: the shared, mutable storage a `var` moves
    /// into when a closure captures it.
    ///
    /// The one value here with reference semantics. Copying one takes another
    /// hold on the same box — deliberately, because a closure and the frame
    /// that wrote the `var` have to see each other's writes — and the last hold
    /// releases what is inside. Everything else on this heap is a value, and
    /// a cell being the exception is the whole of the feature.
    Cell(CellId),
    /// An opaque, target-width pointer word from a foreign (`@FFI.Extern`) call.
    ///
    /// Inline and `Copy` like the other scalars: it owns no heap storage, and
    /// the VM never dereferences, does arithmetic on, or frees it. It only ever
    /// arrives from and returns to the foreign seam
    /// ([`kira_runtime_abi::HostCapabilities::call_foreign`]).
    RawPtr(u64),
    /// An opaque owning handle to native callback state.
    NativeState(NativeStateToken),
    /// A typed mutable view through an opaque callback-state token.
    NativeView {
        /// The stable userdata token.
        token: NativeStateToken,
        /// The type identity recovery validated.
        type_id: NativeStateTypeId,
    },
    /// An aggregate read out of callback state, not yet rebuilt as objects.
    ///
    /// The value semantics of a read are already complete when this exists: the
    /// stored node shares its children with the read, and a later write to the
    /// state gives the writer children of its own, so what this holds is what
    /// was read. Rebuilding it as heap objects is the part that is deferred, and
    /// most reads never need it — a walk over a UI tree reads scalars out of the
    /// leaves and never asks for an object at all.
    NativeSnapshot(SnapshotId),
    /// A uniquely owned block of C storage: a NUL-terminated string, a
    /// C-layout image, or an array flattened to C widths, built for the
    /// foreign seam.
    ///
    /// The block's payload address is what C reads; the block lives exactly as
    /// long as the Kira value holding this handle. A true copy deep-clones the
    /// bytes ([`Heap::copy_value`]), dropping the value frees them, and a
    /// `retains:` parameter transfers them to the heap's retained registry —
    /// so no reference count exists and no storage outlives its owner.
    CBlock(CBlockId),
    /// The unit value.
    Void,
}

/// One child block owned by a C-layout image in the VM heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct VmCBlockChild {
    /// Byte offset of the child pointer in the parent's payload.
    offset: kira_runtime_abi::CBlockOffset,
    /// Width of that pointer in the target C layout.
    width: kira_runtime_abi::ForeignPointerWidth,
    /// The uniquely owned child block.
    block: CBlockId,
}

/// What a heap slot holds.
#[derive(Debug, Clone, PartialEq)]
enum Object {
    /// A string's bytes.
    Str(String),
    /// A struct's fields, in declaration order, shared until one holder writes.
    ///
    /// Shared for the reason an array's elements are, and it is the same
    /// bargain: reading a struct copied every field, and every string, array,
    /// enum and struct inside every field, and then dropped them all again. A
    /// widget tree is structs of structs, so a frame that walks one paid for the
    /// whole tree at every level. `Heap::copy_value` was 10% of an editor frame
    /// and `Heap::free_struct` another 6.6%, measured; a copy is a count now and
    /// [`Heap::make_struct_unique`] buys fields of its own for the first writer.
    Struct(Rc<Vec<Value>>),
    /// An array's elements, in order, shared until one holder writes.
    Array(Rc<Vec<Value>>),
    /// An enum value: a discriminant tag and its optional single payload,
    /// shared by every value that copied it.
    ///
    /// Shared for the same reason an array's elements are, and more simply: an
    /// enum object is never written through — a variant is replaced whole, and
    /// every read of one hands back an owned copy — so a copy needs no object
    /// of its own and there is nothing to make unique.
    Enum {
        /// The variant's declaration index.
        tag: u64,
        /// The payload value, absent for a payload-less variant.
        payload: Option<Value>,
        /// How many values hold this object; the payload goes with the last.
        shares: u32,
    },
    /// An erased value: the type it had before `Any` took it away, and the
    /// value itself.
    ///
    /// Shared and never written through, exactly as an enum object is, and for
    /// the same reason: nothing may write through an `Any`, so a copy needs no
    /// object of its own.
    ///
    /// The VM would not need a box here to *carry* an erased value — its
    /// values are tagged already, which is why erasure emitted no instruction
    /// before this existed. It needs one to *compare* them. A struct object on
    /// this heap is a tuple of values with no record of which declaration built
    /// it, so without the id written here `Point(1, 2)` and `Rect(1, 2)` would
    /// compare equal on the VM and could not compare at all on native. See
    /// [`kira_semantics_model::ErasedTypeId`].
    Erased {
        /// The [`kira_semantics_model::ErasedTypeId`] word of the type that
        /// crossed in.
        type_id: u64,
        /// The value that crossed in.
        payload: Value,
        /// How many values hold this object; the payload goes with the last.
        shares: u32,
    },
    /// A capture cell: one value, shared by every holder, and written through.
    ///
    /// The one *mutable* shared object on this heap. An enum is shared and
    /// never written through, and an array's elements are shared only until a
    /// writer buys its own; a cell is shared precisely so that a write through
    /// one holder is visible through the others.
    Cell {
        /// What the cell holds. Replaced whole by a write, never edited in
        /// place — see [`Heap::cell_set`].
        payload: Value,
        /// How many values hold this box; the payload goes with the last.
        shares: u32,
    },
    /// An aggregate read out of callback state, held as the store's own node.
    ///
    /// Shared and never written through, exactly as an enum object is: the node
    /// is what a read produced, and a write through the state does not reach it
    /// (see [`kira_runtime_abi::NativeStateValue`]). Anything that would edit
    /// what this holds rebuilds it as objects first — [`Heap::own`] — so a
    /// snapshot is only ever read.
    Snapshot {
        /// The node this read landed on.
        node: NativeStateValue,
        /// How many values hold this object.
        shares: u32,
    },
    /// A uniquely owned block of C storage; see [`Value::CBlock`].
    ///
    /// The bytes are boxed so the payload address a foreign callee was handed
    /// stays put while the slot table grows. Unlike every shared kind above,
    /// this one is never counted: exactly one value owns it, a copy clones the
    /// bytes, and the drop frees them — the seam contract in
    /// [`kira_runtime_abi::c_storage`].
    CBlock {
        /// The bytes C reads, at a stable address.
        bytes: Box<[u8]>,
        /// Child blocks whose addresses are embedded in this payload.
        children: Vec<VmCBlockChild>,
    },
}

/// A snapshot of heap allocation counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeapStats {
    /// Total string allocations performed over the run.
    pub allocated: u64,
    /// Total string frees performed over the run.
    pub freed: u64,
    /// Live strings right now (`allocated - freed`).
    pub current: u64,
    /// Values a `retains:` foreign parameter transferred to the heap.
    ///
    /// Counted apart from `current` because they are alive by contract — C
    /// holds their pointers until instance teardown — so a program that exits
    /// with `current == retained` balanced everything it still owned.
    pub retained: u64,
}

/// The object heap: owns every live string and struct, and counts allocations
/// and frees.
///
/// Strings and structs share one slot table and one pair of counters, so
/// `current == 0` at exit proves *both* kinds balanced rather than only one.
#[derive(Debug, Default)]
pub struct Heap {
    slots: Vec<Option<Object>>,
    free_list: Vec<u32>,
    allocated: u64,
    freed: u64,
    released_cells: Arc<ReleasedCells>,
    /// Values a `retains:` foreign parameter transferred here.
    ///
    /// C holds pointers into their C blocks, so they stay alive — and their
    /// heap slots stay occupied — until the whole heap drops at instance
    /// teardown, which never overlaps a foreign call in flight.
    retained: Vec<Value>,
}

/// Cells a callback-state tree gave up its last share of.
///
/// A tree node is dropped by code that has no heap to release against —
/// `Arc::make_mut` unsharing a level, a store entry going away, a snapshot being
/// freed — so the release is *recorded* here and performed by
/// [`Heap::drain_released_cells`]. Late, never early: the share is still held
/// until the drain runs, so nothing can read a cell the tree let go of and find
/// it freed.
///
/// `pending` is what makes draining once per instruction affordable: the empty
/// case is one relaxed load, and the lock is taken only when there is something
/// under it.
#[derive(Debug, Default)]
struct ReleasedCells {
    pending: AtomicUsize,
    handles: Mutex<Vec<u32>>,
}

impl Heap {
    /// Creates an empty heap.
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates `value` on the heap, returning its handle.
    pub fn alloc(&mut self, value: String) -> StrId {
        StrId(self.alloc_object(Object::Str(value)))
    }

    /// Allocates a struct of `fields` on the heap, returning its handle.
    ///
    /// The fields are taken, not copied: whatever produced them (the operand
    /// stack) hands over ownership, exactly as a `Str` handle is handed over.
    pub fn alloc_struct(&mut self, fields: Vec<Value>) -> StructId {
        let fields = self.own_all(fields);
        StructId(self.alloc_object(Object::Struct(Rc::new(fields))))
    }

    /// Allocates an array of `elements` on the heap, returning its handle.
    ///
    /// As with a struct, the elements are taken rather than copied: whatever
    /// produced them hands over ownership.
    pub fn alloc_array(&mut self, elements: Vec<Value>) -> ArrayId {
        let elements = self.own_all(elements);
        ArrayId(self.alloc_object(Object::Array(Rc::new(elements))))
    }

    /// Holds `node` as a deferred read, or builds it now if it is a scalar.
    ///
    /// A scalar is cheaper as a value than as a handle — an `Int` read out of
    /// state is an `Int` — so only an aggregate becomes a snapshot. That is also
    /// what keeps the deferral invisible: every instruction that computes with a
    /// value gets a value, and only the ones that *navigate* one meet a
    /// snapshot.
    pub fn read_state_node(&mut self, node: NativeStateValue) -> Value {
        match node {
            NativeStateValue::Struct(_) | NativeStateValue::Array(_) => Value::NativeSnapshot(
                SnapshotId(self.alloc_object(Object::Snapshot { node, shares: 1 })),
            ),
            // An enum is a snapshot too: reading its tag is the common case and
            // needs no object, and its payload is another node.
            NativeStateValue::Enum { .. } => Value::NativeSnapshot(SnapshotId(
                self.alloc_object(Object::Snapshot { node, shares: 1 }),
            )),
            scalar => self.from_native_state(&scalar),
        }
    }

    /// The node behind a snapshot handle.
    pub fn snapshot_node(&self, id: SnapshotId) -> Option<&NativeStateValue> {
        match self.slots.get(id.0 as usize) {
            Some(Some(Object::Snapshot { node, .. })) => Some(node),
            _ => None,
        }
    }

    /// Releases one hold on a snapshot, freeing the node with the last.
    ///
    /// A handle that names no snapshot frees nothing, exactly as
    /// [`Heap::free_struct`] does for one that names no struct.
    pub fn free_snapshot(&mut self, id: SnapshotId) {
        // Another value still reads this node, so it stays.
        if let Some(Some(Object::Snapshot { shares, .. })) = self.slots.get_mut(id.0 as usize)
            && *shares > 1
        {
            *shares -= 1;
            return;
        }
        let taken = match self.slots.get_mut(id.0 as usize) {
            Some(slot @ Some(Object::Snapshot { .. })) => slot.take(),
            _ => None,
        };
        if taken.is_some() {
            self.freed += 1;
            self.free_list.push(id.0);
        }
    }

    /// Rebuilds a deferred read as heap objects, leaving anything else alone.
    ///
    /// This is where the deferral ends. Every route by which a value could be
    /// *edited* or stored inside something editable goes through here first, so
    /// a snapshot is never reachable from an aggregate and never written
    /// through — which is what lets the read that produced it copy nothing.
    pub fn own(&mut self, value: Value) -> Value {
        let Value::NativeSnapshot(id) = value else {
            return value;
        };
        let node = match self.snapshot_node(id) {
            Some(node) => node.clone(),
            // A handle naming no snapshot rebuilds as the unit value rather
            // than trapping: the slot is still freed below, so the accounting
            // balances either way.
            None => {
                self.free_snapshot(id);
                return Value::Void;
            }
        };
        let rebuilt = self.from_native_state(&node);
        self.free_snapshot(id);
        rebuilt
    }

    /// [`Heap::own`] over a list, in place.
    fn own_all(&mut self, values: Vec<Value>) -> Vec<Value> {
        if !values
            .iter()
            .any(|value| matches!(value, Value::NativeSnapshot(_)))
        {
            return values;
        }
        values.into_iter().map(|value| self.own(value)).collect()
    }
}

mod array;
mod cell;
mod object;
mod variant;

mod aggregate;
mod equality;
mod native_state;
mod seam;

pub use aggregate::AggregateMismatch;

#[cfg(test)]
#[path = "../value_tests.rs"]
mod tests;
