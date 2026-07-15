//! The VM heap: managed-object records, the pointer-keyed registry, and
//! allocation statistics. Ownership is affine (Rust-like): every managed
//! object has exactly one owner; drops are explicit and recursive.
//!
//! Ported from kira-zig `packages/kira_vm_runtime/src/ownership.zig`.
//! This is a field-accurate scaffold: types and layouts only, no alloc/drop
//! logic yet.

use crate::abi::{BridgeValue, Value};

/// Heap-allocated array object header.
///
/// Zig: `ArrayObject` (`extern struct`) — the layout is C-ABI and shared
/// byte-for-byte with `KiraArray` in
/// `packages/kira_native_bridge/src/runtime_helpers.c`, so `#[repr(C)]` and
/// the exact field order are load-bearing.
#[repr(C)]
#[derive(Debug)]
pub struct ArrayObject {
    /// Zig: `len: usize` — number of live elements.
    pub len: usize,
    /// Zig: `items: [*]runtime_abi.BridgeValue` — element storage.
    pub items: *mut BridgeValue,
    /// Zig: `cap: usize`. Invariant (shared with the C bridge's `KiraArray`):
    /// the `items` allocation is always exactly `max(cap, 1)` elements and
    /// `len <= cap`. Appends grow geometrically; every free site reconstructs
    /// the slice from `cap`, not `len`.
    pub cap: usize,
}

/// Heap-allocated closure object.
///
/// Zig: `ClosureObject` — a PLAIN Zig struct (not `extern`), so this side is
/// not layout-pinned; only the VM touches it.
#[derive(Debug)]
pub struct ClosureObject {
    /// Zig: `function_id: u32` — bytecode function the closure invokes.
    pub function_id: u32,
    /// Zig: `is_native: bool = false` — true for closures wrapping a native
    /// function pointer instead of a bytecode function.
    pub is_native: bool,
    /// Zig: `captures: []runtime_abi.Value` — owned captured slots.
    pub captures: Box<[Value]>,
}

/// Struct payload record.
///
/// Zig: `StructFieldsObject` — plain struct; the registry key is the address
/// of the `fields` allocation (empty structs are given one managed void slot
/// so the key is never null).
#[derive(Debug)]
pub struct StructFieldsObject {
    /// Zig: `type_name: []const u8` — borrowed from the loaded module's
    /// constant pool (lives as long as the module, including retired modules
    /// after hot swap). TODO(port): borrow or intern instead of owning.
    pub type_name: String,
    /// Zig: `fields: []runtime_abi.Value` — owned field slots.
    pub fields: Box<[Value]>,
}

/// What a registry record points at.
/// Zig: `ObjectKind` (`union(enum)`). The `StringBytes` arm is the heap
/// string representation: an owned byte buffer registered under its data
/// pointer (`registerString`); empty strings are never registered.
#[derive(Debug)]
pub enum ObjectKind {
    /// Zig: `array: *ArrayObject`.
    Array(*mut ArrayObject),
    /// Zig: `closure: *ClosureObject`.
    Closure(*mut ClosureObject),
    /// Zig: `struct_fields: StructFieldsObject` (inline, keyed by fields ptr).
    StructFields(StructFieldsObject),
    /// Zig: `string_bytes: []u8` — owned string bytes, keyed by data ptr.
    StringBytes(Box<[u8]>),
}

/// Who allocated an object. Zig: `ObjectOrigin`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ObjectOrigin {
    /// Allocated by VM execution. Zig: `.runtime_alloc`.
    #[default]
    RuntimeAlloc,
    /// Materialized from a native-layout value crossing the bridge.
    /// Zig: `.native_materialize`.
    NativeMaterialize,
}

/// One registry entry. Zig: `ObjectRecord`.
#[derive(Debug)]
pub struct ObjectRecord {
    /// Zig: `origin: ObjectOrigin = .runtime_alloc`.
    pub origin: ObjectOrigin,
    /// Zig: `kind: ObjectKind`.
    pub kind: ObjectKind,
}

/// Heap allocation statistics. Zig: `HeapStats` (all fields `usize`,
/// default 0). `*_current` tracks live objects, `*_peak` the high-water mark,
/// `*_allocated`/`*_freed` are monotonic counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeapStats {
    pub arrays_current: usize,
    pub arrays_peak: usize,
    pub arrays_allocated: usize,
    pub arrays_freed: usize,
    pub closures_current: usize,
    pub closures_peak: usize,
    pub closures_allocated: usize,
    pub closures_freed: usize,
    pub structs_current: usize,
    pub structs_peak: usize,
    pub structs_allocated: usize,
    pub structs_freed: usize,
    pub strings_current: usize,
    pub strings_peak: usize,
    pub strings_allocated: usize,
    pub strings_freed: usize,
}

/// Purpose-built registry map for managed-object pointers.
///
/// Zig: `PointerObjectMap`. The heap registry is the hottest data structure
/// of allocation-heavy VM workloads: every alloc registers, every drop
/// removes, and every is-managed/ownership check probes — including misses
/// for foreign (native) pointers. The port keeps the Zig design:
///
/// - keys are non-zero pointers; 0 marks an empty slot,
/// - one cheap two-multiply mix (Murmur3 finalizer) instead of a general
///   hasher,
/// - split key/record arrays so probing touches a dense 8-byte key lane,
/// - linear probing with backward-shift deletion: no tombstones, so heavy
///   register/drop churn never degrades probes or forces cleanup rehashes,
/// - growth at 2/3 load, capacity always a power of two (min 64).
///
/// Scaffold: fields only; probe/insert/remove logic lands with the port.
#[derive(Debug, Default)]
pub struct PointerObjectMap {
    /// Zig: `keys: []usize` — dense key lane, 0 = empty slot.
    pub keys: Vec<usize>,
    /// Zig: `records: []ObjectRecord` — record for the key at the same slot.
    /// Only slots whose key is non-zero hold initialized records; the port
    /// will keep them as `MaybeUninit` (or an option-free parallel array) in
    /// the unsafe core.
    pub records: Vec<Option<ObjectRecord>>,
    /// Zig: `len: usize` — live entry count.
    pub len: usize,
}

/// The VM heap: registry + pin frames + free-list pools.
///
/// Zig: `Heap` in `ownership.zig`. Scaffold notes carried from Zig:
/// - `leak_heap` memoizes the `KIRA_LEAK_HEAP` diagnostic env var ONCE at
///   init (`dropPtr` runs ~140k times per hybrid UI frame; a getenv per drop
///   was measurable).
/// - The slice pools cache the allocation shapes the VM churns hardest
///   (struct field slices, array backing stores, object headers). Pooled
///   blocks are real allocator allocations of exactly the bucket size, so
///   pooled and non-pooled call sites stay interchangeable.
/// - Pool tuning: slices pooled up to len 64 (`max_pooled_slice_len`; wide
///   all-scalar UI structs), at most 1024 entries per bucket
///   (`max_pool_entries`).
#[derive(Debug, Default)]
pub struct Heap {
    /// Zig: `objects: PointerObjectMap` — the managed-object registry.
    pub objects: PointerObjectMap,
    /// Zig: `pin_frames` — boundary pin scopes; each frame pins the object
    /// graphs of values handed across a native boundary so a re-entrant drop
    /// cannot free them mid-call.
    pub pin_frames: Vec<PinFrame>,
    /// Zig: `stats: HeapStats = .{}`.
    pub stats: HeapStats,
    /// Zig: `leak_heap: bool` — KIRA_LEAK_HEAP diagnostic: leak every object
    /// instead of destroying it.
    pub leak_heap: bool,
    // TODO(port): value_slice_pools / bridge_slice_pools /
    // array_object_pool / closure_object_pool free-lists land with the
    // allocator story (bumpalo or a raw-alloc pool) in the unsafe core.
}

/// One boundary pin scope. Zig: private `PinFrame` (`pinned` pointer set).
#[derive(Debug, Default)]
pub struct PinFrame {
    /// Registry keys pinned for the duration of this scope.
    pub pinned: std::collections::HashSet<usize>,
}
