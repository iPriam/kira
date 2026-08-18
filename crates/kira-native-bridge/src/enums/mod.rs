//! The native enum runtime: a boxed tag plus a type-erased payload, with the
//! affine clone/free pair.
//!
//! # ABI
//!
//! A Kira enum crosses the boundary as a [`KEnum`]: an *opaque owned handle*,
//! one pointer, never an aggregate — the same discipline [`crate::runtime`]'s
//! `KStr` and [`crate::array`]'s `KArray` follow, and for the same reason. The
//! box behind it is `#[repr(C)]` because the backend and this crate are
//! compiled separately and have to agree on it.
//!
//! ```text
//!   tag           the variant's discriminant, the value `==` compares
//!   payload_kind  what `payload` is, and so what clone/free owe it
//!   payload       the variant's single payload, type-erased into one word
//! ```
//!
//! # Why the payload is one word plus a kind, not a type
//!
//! The box is generic over the variant's payload type. A scalar (`Int`,
//! `Float`, `Bool`) fits one word directly — the backend passes its bits and
//! this code copies them, owning nothing. A `String` payload is an owned `KStr`
//! handle, and a *nested enum* payload is an owned [`KEnum`] handle. A struct or
//! array is wider, so its word points to aligned erased bytes plus clone/free
//! leaves.
//! [`KiraEnum::payload_kind`] lets one clone/free pair serve every case without
//! carrying compiler type metadata: the kind says how to interpret and reclaim
//! that word.
//!
//! A nested enum is what `Result`-shaped values are made of — `Error` carries
//! the failure enum — so `attempt`/`try`/`handle` is the construct that needs
//! [`PAYLOAD_ENUM`]. A struct or array payload uses [`PAYLOAD_AGGREGATE`]: its
//! word points to aligned bytes plus compiler-generated clone/free leaves,
//! allowing recursive construct-family values without baking compiler type
//! metadata into runtime.
//!
//! # Ownership
//!
//! Affine, mirroring the VM's heap: reading an enum copies it
//! ([`kira_rt_enum_clone`]), and a local leaving scope or being overwritten
//! frees it ([`kira_rt_enum_free`]). A well-formed program frees every
//! allocation exactly once — the guarantee the VM proves with its heap
//! accounting.
//!
//! # A copy is a share
//!
//! **A box is never written through.** Every read here — [`kira_rt_enum_tag`],
//! [`kira_rt_enum_payload`], [`kira_rt_enum_payload_aggregate`] — leaves the box
//! exactly as it found it and hands back a value the caller owns, because a
//! variant is replaced whole rather than edited. So a copy needs no new box at
//! all: [`kira_rt_enum_clone`] adds a share and gives back the same handle, and
//! [`kira_rt_enum_free`] releases the payload with the last of them.
//!
//! That is the whole of it — no make-unique, and nothing for the backend to
//! route, unlike [`crate::array`], where a block *is* written through. What it
//! removes is a `malloc`, a deep copy of the payload, and a matching `free` on
//! every read of every enum: a layout descriptor's sizing modes and alignments
//! are payload-less and were already free, but its insets, colours and text
//! runs are not, and they were the largest cost left in a Project Matter frame.
//!
//! The count is a plain `usize`, not an atomic: the runtime is single-threaded,
//! as its string and array storage already assume.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use std::alloc::{self, Layout};

use kira_runtime_abi::EnumPayloadKind;

use crate::array::{ElemClone, ElemFree};
use crate::pool::SharedPool;
use crate::runtime::{KStr, kira_rt_str_clone, kira_rt_str_free};

/// The free list enum boxes are handed out from.
///
/// One box per enum value a program *constructs* — a copy takes no box at all
/// now — and a UI frame that rebuilds its view tree constructs thousands. See
/// [`crate::pool`] for what a `static` free list assumes.
static BOXES: SharedPool = SharedPool::new(Layout::new::<KiraEnum>());

/// A Kira enum at the native ABI: an opaque owned handle.
pub type KEnum = *mut KiraEnum;

/// Whether `value` carries its whole meaning in the handle.
///
/// A variant with no payload is nothing but a tag, and a tag fits in the handle
/// — so it is stored there, as `(tag << 1) | 1`, and no box is allocated for
/// it. A real box comes from the allocator word-aligned, so the low bit is free
/// to say which is which.
///
/// This is not a micro-optimization. A layout descriptor is mostly payload-less
/// variants (an axis, an alignment, a sizing mode), each of which used to cost
/// a `malloc` on construction and another on every read that cloned it —
/// measured as the largest remaining cost in a Project Matter frame once state
/// stopped being copied.
#[must_use]
pub fn is_inline(value: KEnum) -> bool {
    value as usize & 1 == 1
}

/// The handle for a payload-less variant with this tag.
#[must_use]
pub fn inline_handle(tag: i64) -> KEnum {
    ((tag as usize) << 1 | 1) as KEnum
}

/// The tag an inline handle carries.
fn inline_tag(value: KEnum) -> i64 {
    (value as usize >> 1) as i64
}

/// Payload word is inert bits (a scalar, or no payload at all); owns nothing.
pub const PAYLOAD_INERT: i64 = EnumPayloadKind::INERT.as_i64();
/// Payload word is an owned [`KStr`] to clone and free with the box.
pub const PAYLOAD_STR: i64 = EnumPayloadKind::STR.as_i64();
/// Payload word is an owned [`KEnum`] to clone and free with the box.
pub const PAYLOAD_ENUM: i64 = EnumPayloadKind::ENUM.as_i64();
/// Payload word points to an owned erased aggregate and its clone/free leaves.
pub const PAYLOAD_AGGREGATE: i64 = EnumPayloadKind::AGGREGATE.as_i64();

/// The heap box behind a [`KEnum`].
///
/// `#[repr(C)]` because the backend, compiled separately, references this
/// layout only through the `kira_rt_enum_*` helpers — but keeping it `repr(C)`
/// makes the intent explicit and the layout stable.
#[repr(C)]
pub struct KiraEnum {
    /// The variant's discriminant.
    tag: i64,
    /// What `payload` is: [`PAYLOAD_INERT`], [`PAYLOAD_STR`],
    /// [`PAYLOAD_ENUM`], or [`PAYLOAD_AGGREGATE`].
    payload_kind: i64,
    /// The variant's single payload, type-erased into one word.
    payload: u64,
    /// How many values hold this box.
    ///
    /// One when it is made, one more per copy, one fewer per free, and the box
    /// and its payload go at zero. Last, so the three fields above keep the
    /// offsets they had.
    shares: usize,
}

impl KiraEnum {
    /// What this box's payload word is, as a [`PAYLOAD_INERT`]-family constant.
    ///
    /// For [`crate::cells`], which shares this box: a cell has to read the kind
    /// back to release the payload it is replacing, and an enum never does
    /// because an enum is never written through.
    pub(crate) fn payload_kind_raw(&self) -> i64 {
        self.payload_kind
    }

    /// This box's payload word, uninterpreted and unowned by the reader.
    pub(crate) fn payload_raw(&self) -> u64 {
        self.payload
    }

    /// Overwrites the payload word and its kind together.
    ///
    /// The two are one fact — a word is meaningless without the kind that says
    /// what it is — so they are written by one method rather than two fields, and
    /// releasing what was there is the caller's job. [`crate::cells`] is the only
    /// caller, and the reason a box is written through at all.
    pub(crate) fn set_payload_raw(&mut self, payload_kind: i64, payload: u64) {
        self.payload_kind = payload_kind;
        self.payload = payload;
    }
}

/// Heap storage behind an aggregate payload word.
///
/// The bytes use the same fixed alignment as native array elements: every Kira
/// field is at most eight-byte aligned, and LLVM's ABI size includes the padding
/// needed to keep nested fields aligned. Clone/free leaves are generated for the
/// concrete payload type, so this runtime never needs compiler type metadata.
pub(crate) struct AggregatePayload {
    data: *mut u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
    /// Compares two payloads of this concrete type, for `Any` equality.
    ///
    /// The third leaf of the same family as `clone` and `free`, and needed for
    /// the same reason: the bytes are untyped here, so only code generated for
    /// the concrete type can read them. Null when the value was boxed by a
    /// caller that predates erasure comparison — an ordinary enum payload,
    /// which no comparison reaches — and a null leaf answers "not equal"
    /// rather than reading bytes it has no layout for.
    eq: Option<ElemEq>,
}

/// Compares two elements of one concrete type.
///
/// Returns non-zero when they are equal. Both pointers stay owned by their
/// boxes: a comparison reads and takes nothing.
pub type ElemEq = unsafe extern "C" fn(a: *const u8, b: *const u8) -> u8;

const AGGREGATE_ALIGN: usize = 8;

fn aggregate_layout(size: usize) -> Option<Layout> {
    if size == 0 {
        return None;
    }
    match Layout::from_size_align(size, AGGREGATE_ALIGN) {
        Ok(layout) => Some(layout),
        Err(_) => std::process::abort(),
    }
}

fn alloc_aggregate_bytes(size: usize) -> *mut u8 {
    match aggregate_layout(size) {
        Some(layout) => {
            // SAFETY: `layout` has non-zero size and valid power-of-two alignment.
            let data = unsafe { alloc::alloc(layout) };
            if data.is_null() {
                alloc::handle_alloc_error(layout);
            }
            data
        }
        None => std::ptr::null_mut(),
    }
}

/// Copies an owned aggregate value's bits into a fresh erased box.
///
/// This is a move, not a clone: the caller hands ownership of every handle in
/// `source` to the returned payload and must not drop the source value again.
///
/// # Safety
/// `source` must point to `size` readable bytes with the concrete aggregate
/// payload type's layout. `clone` and `free`, when present, must operate on
/// that same type.
pub(crate) unsafe fn move_aggregate(
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
    eq: Option<ElemEq>,
) -> *mut AggregatePayload {
    let data = alloc_aggregate_bytes(size);
    if size > 0 {
        // SAFETY: caller supplies `size` readable bytes and `data` is a distinct
        // allocation of exactly that size.
        unsafe { std::ptr::copy_nonoverlapping(source, data, size) };
    }
    // One count for the whole erased payload: the box and its bytes are
    // allocated together and reclaimed together in `free_aggregate`, so
    // counting them once keeps the pair one-to-one.
    crate::accounting::record_alloc();
    Box::into_raw(Box::new(AggregatePayload {
        data,
        size,
        clone,
        free,
        eq,
    }))
}

/// Writes an independent aggregate copy into caller-owned storage.
///
/// # Safety
/// `value` must be a live aggregate payload and `out` must point to `size`
/// writable bytes with the payload's concrete alignment.
pub(crate) unsafe fn read_aggregate(value: *mut AggregatePayload, out: *mut u8) {
    if value.is_null() {
        return;
    }
    // SAFETY: caller guarantees a live payload that outlives this read.
    let source = unsafe { &*value };
    if source.size == 0 {
        return;
    }
    // SAFETY: `out` covers the payload's size and cannot overlap its private box.
    unsafe { std::ptr::copy_nonoverlapping(source.data, out, source.size) };
    if let Some(clone) = source.clone {
        // SAFETY: callback and pointers carry the concrete payload type.
        unsafe { clone(source.data, out) };
    }
}

/// Releases an erased aggregate payload.
///
/// # Safety
/// `value` must be null or a live payload, freed at most once.
pub(crate) unsafe fn free_aggregate(value: *mut AggregatePayload) {
    if value.is_null() {
        return;
    }
    crate::accounting::record_free();
    // SAFETY: caller's free-once contract makes this the only reclaim.
    let value = unsafe { Box::from_raw(value) };
    if !value.data.is_null() {
        if let Some(free) = value.free {
            // SAFETY: callback was emitted for the concrete payload type.
            unsafe { free(value.data) };
        }
        if let Some(layout) = aggregate_layout(value.size) {
            // SAFETY: `data` came from `alloc` with exactly this layout.
            unsafe { alloc::dealloc(value.data, layout) };
        }
    }
}

/// Boxes a fresh enum value.
///
/// `payload_kind` says what the box takes ownership of: [`PAYLOAD_INERT`] for
/// scalar bits or no payload at all, [`PAYLOAD_STR`] for an owned `KStr`,
/// [`PAYLOAD_ENUM`] for an owned nested `KEnum`. An unrecognized kind is
/// treated as inert, which leaks rather than corrupting — the conservative
/// direction for a word this code cannot interpret.
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_enum_new(tag: i64, payload_kind: i64, payload: u64) -> KEnum {
    crate::accounting::record_alloc();
    let boxed = BOXES.alloc().cast::<KiraEnum>();
    // SAFETY: the pool hands back a block of exactly this layout, and every
    // field is written before anything reads one.
    unsafe {
        boxed.write(KiraEnum {
            tag,
            payload_kind,
            payload,
            shares: 1,
        });
    }
    boxed
}

/// Boxes an aggregate payload by moving its bytes into erased runtime storage.
///
/// Appended beside [`kira_rt_enum_new`] rather than changing its signature, so
/// existing enum callers and runtime archives keep their ABI. The backend passes
/// clone/free leaves for the concrete aggregate type; null means a flat bit-copy is
/// sufficient or nothing owns storage.
///
/// # Safety
/// `source` must point to `size` readable bytes of one live Kira aggregate value.
/// The value's ownership is transferred to the returned enum and must not be
/// released through `source` afterwards. Callbacks must match that aggregate type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_new_aggregate(
    tag: i64,
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
) -> KEnum {
    // SAFETY: caller provides the concrete payload's bytes and matching leaves.
    let payload = unsafe { move_aggregate(source, size, clone, free, None) };
    kira_rt_enum_new(tag, PAYLOAD_AGGREGATE, payload as u64)
}

/// Boxes an aggregate payload that can also be compared.
///
/// Appended beside [`kira_rt_enum_new_aggregate`] rather than changing its
/// signature, so existing callers and runtime archives keep their ABI. Erasure
/// uses this one; an ordinary enum payload uses the older one and gets a null
/// `eq`, which no comparison reaches.
///
/// # Safety
/// As [`kira_rt_enum_new_aggregate`], and `eq` must have been generated for the
/// same concrete type as `clone` and `free`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_new_aggregate_eq(
    tag: i64,
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
    eq: Option<ElemEq>,
) -> KEnum {
    // SAFETY: caller provides the concrete payload's bytes and matching leaves.
    let payload = unsafe { move_aggregate(source, size, clone, free, eq) };
    kira_rt_enum_new(tag, PAYLOAD_AGGREGATE, payload as u64)
}

/// Whether two erased values are structurally equal.
///
/// What `EqAny` lowers to. The tag of an erasure box is its
/// `kira_semantics_model::ErasedTypeId` word rather than a variant
/// discriminant, so two boxes whose tags differ hold different Kira types and
/// are unequal without their payloads being read at all. That test is what
/// makes reading them afterwards sound: an aggregate is untyped bytes here, and
/// reading a `Rect`'s through a `Point`'s leaf would be undefined behavior
/// rather than a wrong answer.
///
/// Both handles stay owned by the caller: comparing takes nothing.
///
/// # Safety
/// `a` and `b` must be null or live handles from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_any_eq(a: KEnum, b: KEnum) -> u8 {
    // SAFETY: the caller's live-handle contract is this function's own.
    u8::from(unsafe { boxes_equal(a, b) })
}

/// The family word an `ErasedTypeId` carries in its high 32 bits.
///
/// Mirrors `kira_semantics_model::ty::erased`'s `kind` module, which is the
/// wire side of this contract. Only the two that need different treatment from
/// a plain word comparison are named: a float compares as a float so `NaN`
/// matches nothing, and every other scalar family is its bits.
const ERASED_FAMILY_FLOAT: i64 = 1;

/// Compares two enum-shaped boxes structurally.
///
/// Used for erasure boxes and for the nested enums they carry, which are the
/// same shape: a tag, and one payload whose kind says what it is.
///
/// # Safety
/// Both must be null or live handles from this runtime.
unsafe fn boxes_equal(a: KEnum, b: KEnum) -> bool {
    if a.is_null() || b.is_null() {
        return a.is_null() && b.is_null();
    }
    // SAFETY: both are live handles, and reading a tag never consumes one.
    let (a_tag, b_tag) = unsafe { (kira_rt_enum_tag(a), kira_rt_enum_tag(b)) };
    if a_tag != b_tag {
        return false;
    }
    // A payload-less variant is its tag and nothing else.
    if is_inline(a) || is_inline(b) {
        return is_inline(a) && is_inline(b);
    }
    // SAFETY: neither is inline, so both address a live `KiraEnum`.
    let (one, other) = unsafe { (&*a, &*b) };
    if one.payload_kind != other.payload_kind {
        return false;
    }
    match one.payload_kind {
        PAYLOAD_INERT => {
            if a_tag >> 32 == ERASED_FAMILY_FLOAT {
                // IEEE, deliberately: making erasure the one place `NaN`
                // compares equal to itself would be a worse surprise than the
                // rule every other float comparison already follows.
                return f64::from_bits(one.payload) == f64::from_bits(other.payload);
            }
            one.payload == other.payload
        }
        // SAFETY: the kind says both words are live `KStr` handles.
        PAYLOAD_STR => unsafe { str_payloads_equal(one.payload, other.payload) },
        // SAFETY: the kind says both words are live nested `KEnum` handles.
        PAYLOAD_ENUM => unsafe { boxes_equal(one.payload as KEnum, other.payload as KEnum) },
        PAYLOAD_AGGREGATE => {
            let (one, other) = (
                one.payload as *mut AggregatePayload,
                other.payload as *mut AggregatePayload,
            );
            if one.is_null() || other.is_null() {
                return one.is_null() && other.is_null();
            }
            // SAFETY: the kind says both words are live aggregate payloads.
            let (one, other) = unsafe { (&*one, &*other) };
            // Equal tags already proved both hold the same Kira type, so the
            // sizes agree and either leaf reads either payload correctly. A
            // null leaf on either side is a payload boxed by a caller that
            // never compares.
            let (Some(eq), Some(_)) = (one.eq, other.eq) else {
                return false;
            };
            // SAFETY: both point at live payloads of the type `eq` was
            // generated for, and the comparison reads without taking.
            unsafe { eq(one.data, other.data) != 0 }
        }
        // An unrecognized kind is a word this code cannot interpret; saying
        // "not equal" is the conservative direction, matching how the same
        // unknown is treated as inert rather than guessed at when freeing.
        _ => false,
    }
}

/// Compares two `KStr` payload words, leaving both boxes as they were found.
///
/// `kira_rt_str_eq` consumes what it compares, so each side is cloned first and
/// the clones are what it releases. The payloads themselves stay owned by the
/// boxes holding them, which is what lets a comparison take nothing.
///
/// # Safety
/// Both words must be live `KStr` handles or null.
unsafe fn str_payloads_equal(one: u64, other: u64) -> bool {
    // SAFETY: caller's live-handle contract, and each clone is handed to
    // `kira_rt_str_eq`, which releases exactly the two it is given.
    unsafe {
        let (a, b) = (
            kira_rt_str_clone(one as KStr),
            kira_rt_str_clone(other as KStr),
        );
        crate::runtime::kira_rt_str_eq(a, b) != 0
    }
}

/// Reads an enum's discriminant tag; leaves the enum untouched.
///
/// A null handle reads as tag 0, which is what a zero-initialized slot holds —
/// the native mirror of the VM initializing every slot to `Void`.
///
/// # Safety
/// `value` must be null or a live handle from this runtime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_tag(value: KEnum) -> i64 {
    if value.is_null() {
        return 0;
    }
    if is_inline(value) {
        return inline_tag(value);
    }
    // SAFETY: a non-null handle is a live `KiraEnum` the caller has not freed.
    unsafe { (*value).tag }
}

/// Reads an enum's payload as an *owned* word, leaving the enum untouched.
///
/// This is what a `match` arm's binding — and a `handle` arm's — reads. A
/// `String` or nested-enum payload is cloned, so the returned handle is the
/// caller's to free and the box still owns its own: the same affine discipline
/// [`kira_rt_enum_clone`] follows. A scalar payload is returned by bits and owns
/// nothing. A null handle reads as 0, mirroring [`kira_rt_enum_tag`].
///
/// The caller knows from the variant's declared payload type which kind of
/// handle it got, which is why the kind does not have to come back with it.
///
/// # Safety
/// `value` must be null or a live handle from this runtime; it is left
/// untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_payload(value: KEnum) -> u64 {
    // An inline handle is a tag and nothing else, so it has no payload to read.
    if value.is_null() || is_inline(value) {
        return 0;
    }
    // SAFETY: a non-null handle is a live `KiraEnum` that outlives this read.
    let source = unsafe { &*value };
    match source.payload_kind {
        // SAFETY: the kind promises `payload` is a live `KStr`; cloning it
        // reads it and leaves it in place.
        PAYLOAD_STR => (unsafe { kira_rt_str_clone(source.payload as KStr) }) as u64,
        // SAFETY: the kind promises `payload` is a live `KEnum`; cloning it
        // reads it and leaves it in place.
        PAYLOAD_ENUM => (unsafe { kira_rt_enum_clone(source.payload as KEnum) }) as u64,
        // Aggregate payloads are wider than one word and use the dedicated
        // out-pointer helper below.
        PAYLOAD_AGGREGATE => 0,
        _ => source.payload,
    }
}

/// Reads an aggregate payload into caller-owned storage as an independent value.
///
/// The enum keeps owning its payload; clone leaves duplicate every nested handle
/// into `out`, so the caller may outlive and free the source enum independently.
///
/// # Safety
/// `value` must be a live enum whose payload kind is [`PAYLOAD_AGGREGATE`]. `out`
/// must point to writable storage for the concrete payload type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_payload_aggregate(value: KEnum, out: *mut u8) {
    if value.is_null() || is_inline(value) {
        return;
    }
    // SAFETY: caller guarantees a live enum handle.
    let source = unsafe { &*value };
    if source.payload_kind != PAYLOAD_AGGREGATE {
        return;
    }
    // SAFETY: the kind promises an aggregate payload and caller provides its
    // concrete destination slot.
    unsafe { read_aggregate(source.payload as *mut AggregatePayload, out) };
}

/// Produces a copy of an enum (copy-on-read for locals): the same box, held
/// once more.
///
/// Independent is a promise about what a reader can observe, and a box has
/// nothing a reader can change — every read hands back an owned value and
/// leaves the box alone — so two values sharing one box behave exactly as two
/// deep copies. A null handle copies to null and an inline handle to itself,
/// neither of which is a box at all.
///
/// # Safety
/// `value` must be null or a live handle; it is left untouched but for its
/// share count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_clone(value: KEnum) -> KEnum {
    if value.is_null() || is_inline(value) {
        return value;
    }
    // SAFETY: a non-null, non-inline handle is a live `KiraEnum`. The count
    // cannot wrap: it rises by one per live value holding this box.
    unsafe { (*value).shares += 1 };
    value
}

/// Releases one hold on an enum, freeing the box and an owned `String`,
/// nested-enum or aggregate payload once no value holds it. A null handle is a
/// no-op.
///
/// # Safety
/// `value` must be null or a live handle from this runtime, released once per
/// copy of it that was made.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_free(value: KEnum) {
    // An inline handle was never allocated, so there is nothing to reclaim.
    if value.is_null() || is_inline(value) {
        return;
    }
    // SAFETY: a non-null, non-inline handle is a live `KiraEnum`.
    let shares = unsafe { &mut (*value).shares };
    if *shares > 1 {
        // Another value still reads this box, so its payload stays.
        *shares -= 1;
        return;
    }
    // SAFETY: this was the last hold on the box, and the caller's release-once
    // contract makes this the only reclaim of it.
    let (payload_kind, payload) = unsafe { ((*value).payload_kind, (*value).payload) };
    crate::accounting::record_free();
    // SAFETY: the box is finished with and nothing reads it again; it owns
    // nothing itself — its payload is released below.
    unsafe { BOXES.free(value.cast::<u8>()) };
    match payload_kind {
        // SAFETY: the kind promises `payload` is a live `KStr`, freed here
        // exactly once as the box is reclaimed.
        PAYLOAD_STR => unsafe { kira_rt_str_free(payload as KStr) },
        // SAFETY: the kind promises `payload` is a live `KEnum`, freed here
        // exactly once as the box is reclaimed. Recursion is bounded by the
        // program's nesting depth, which is finite because a payload type
        // resolves against types that already resolve.
        PAYLOAD_ENUM => unsafe { kira_rt_enum_free(payload as KEnum) },
        // SAFETY: the kind promises a live aggregate payload, owned exactly once
        // by this enum box.
        PAYLOAD_AGGREGATE => unsafe { free_aggregate(payload as *mut AggregatePayload) },
        _ => {}
    }
}

#[cfg(test)]
mod tests;
