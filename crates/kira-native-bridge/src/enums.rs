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
//! handle, and a *nested enum* payload is an owned [`KEnum`] handle. A struct is
//! wider, so its word points to aligned erased bytes plus clone/free leaves.
//! [`KiraEnum::payload_kind`] lets one clone/free pair serve every case without
//! carrying compiler type metadata: the kind says how to interpret and reclaim
//! that word.
//!
//! A nested enum is what `Result`-shaped values are made of — `Error` carries
//! the failure enum — so `attempt`/`try`/`handle` is the construct that needs
//! [`PAYLOAD_ENUM`]. A struct payload uses [`PAYLOAD_AGGREGATE`]: its word points
//! to aligned bytes plus compiler-generated clone/free leaves, allowing recursive
//! construct-family values without baking compiler type metadata into runtime.
//! Array payloads remain refused at declaration time.
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

/// Heap storage behind an aggregate payload word.
///
/// The bytes use the same fixed alignment as native array elements: every Kira
/// field is at most eight-byte aligned, and LLVM's ABI size includes the padding
/// needed to keep nested fields aligned. Clone/free leaves are generated for the
/// concrete payload type, so this runtime never needs compiler type metadata.
struct AggregatePayload {
    data: *mut u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
}

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
/// `source` must point to `size` readable bytes with the concrete payload type's
/// layout. `clone` and `free`, when present, must operate on that same type.
unsafe fn move_aggregate(
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
) -> *mut AggregatePayload {
    let data = alloc_aggregate_bytes(size);
    if size > 0 {
        // SAFETY: caller supplies `size` readable bytes and `data` is a distinct
        // allocation of exactly that size.
        unsafe { std::ptr::copy_nonoverlapping(source, data, size) };
    }
    Box::into_raw(Box::new(AggregatePayload {
        data,
        size,
        clone,
        free,
    }))
}

/// Writes an independent aggregate copy into caller-owned storage.
///
/// # Safety
/// `value` must be a live aggregate payload and `out` must point to `size`
/// writable bytes with the payload's concrete alignment.
unsafe fn read_aggregate(value: *mut AggregatePayload, out: *mut u8) {
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
unsafe fn free_aggregate(value: *mut AggregatePayload) {
    if value.is_null() {
        return;
    }
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

/// Boxes a struct payload by moving its bytes into erased runtime storage.
///
/// Appended beside [`kira_rt_enum_new`] rather than changing its signature, so
/// existing enum callers and runtime archives keep their ABI. The backend passes
/// clone/free leaves for the concrete struct type; null means a flat bit-copy is
/// sufficient or nothing owns storage.
///
/// # Safety
/// `source` must point to `size` readable bytes of one live Kira struct value.
/// The value's ownership is transferred to the returned enum and must not be
/// released through `source` afterwards. Callbacks must match that struct type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_new_aggregate(
    tag: i64,
    source: *const u8,
    size: usize,
    clone: Option<ElemClone>,
    free: Option<ElemFree>,
) -> KEnum {
    // SAFETY: caller provides the concrete payload's bytes and matching leaves.
    let payload = unsafe { move_aggregate(source, size, clone, free) };
    kira_rt_enum_new(tag, PAYLOAD_AGGREGATE, payload as u64)
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

/// Reads a struct payload into caller-owned storage as an independent value.
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
mod tests {
    use super::*;

    /// Builds a string handle, as the backend's lowering would.
    fn str_handle(text: &str) -> KStr {
        // SAFETY: the slice covers exactly `len` readable bytes.
        unsafe { crate::runtime::kira_rt_str_new(text.as_ptr(), text.len()) }
    }

    #[test]
    fn a_scalar_enum_round_trips_its_tag_and_frees_cleanly() {
        // SAFETY: the handle is live and released once per copy of it.
        unsafe {
            let value = kira_rt_enum_new(2, PAYLOAD_INERT, 42);
            assert_eq!(kira_rt_enum_tag(value), 2);
            let copy = kira_rt_enum_clone(value);
            assert_eq!(kira_rt_enum_tag(copy), 2);
            assert_eq!(value, copy, "a copy is the same box, held twice");
            assert_eq!((*value).shares, 2);
            kira_rt_enum_free(value);
            kira_rt_enum_free(copy);
        }
    }

    /// A copy holds the box rather than duplicating its payload, and the payload
    /// outlives every hold but the last.
    ///
    /// Under Miri or ASan, releasing the payload with the first hold would
    /// surface as a use-after-free on the read below; never releasing it would
    /// surface as a leak.
    #[test]
    fn a_string_payload_is_shared_and_freed_with_the_last_hold() {
        // SAFETY: every handle below is live and released once per hold.
        unsafe {
            let value = kira_rt_enum_new(0, PAYLOAD_STR, str_handle("payload") as u64);
            let copy = kira_rt_enum_clone(value);
            assert_eq!(value, copy, "a copy is the same box");

            kira_rt_enum_free(value);
            let read = kira_rt_enum_payload(copy) as KStr;
            assert_eq!(
                crate::runtime::kira_rt_str_len(read),
                7,
                "the payload survived the first hold"
            );
            kira_rt_str_free(read);
            kira_rt_enum_free(copy);
        }
    }

    #[test]
    fn a_payload_read_is_owned_and_leaves_the_enum_intact() {
        // What a `match` binding does: read the payload, then free the enum.
        // The read string must survive that, and freeing it must not double-free
        // the box's own — the affine guarantee the VM proves with heap counters.
        // SAFETY: every handle below is live and freed exactly once.
        unsafe {
            let value = kira_rt_enum_new(1, PAYLOAD_STR, str_handle("bound") as u64);
            let read = kira_rt_enum_payload(value) as KStr;
            // The read is the caller's to release, which is what matters — a
            // string is shared rather than duplicated, so it *is* the box's
            // handle, held once more. Releasing the box must leave it readable.
            kira_rt_enum_free(value);
            assert_eq!(
                crate::runtime::kira_rt_str_len(read),
                5,
                "the binding outlived the enum it came from"
            );
            kira_rt_str_free(read);

            let scalar = kira_rt_enum_new(0, PAYLOAD_INERT, 77);
            assert_eq!(kira_rt_enum_payload(scalar), 77);
            kira_rt_enum_free(scalar);
        }
    }

    #[repr(C)]
    struct AggregateFixture {
        count: i64,
        label: KStr,
    }

    unsafe extern "C" fn clone_fixture(source: *const u8, target: *mut u8) {
        // SAFETY: test passes pointers to aligned `AggregateFixture` values.
        let (source, target) = unsafe {
            (
                &*(source.cast::<AggregateFixture>()),
                &mut *(target.cast::<AggregateFixture>()),
            )
        };
        // SAFETY: source label remains live for the duration of this clone.
        target.label = unsafe { kira_rt_str_clone(source.label) };
    }

    unsafe extern "C" fn free_fixture(value: *mut u8) {
        // SAFETY: test passes an aligned `AggregateFixture` slot exactly once.
        let value = unsafe { &mut *value.cast::<AggregateFixture>() };
        // SAFETY: the fixture owns this live label exactly once.
        unsafe { kira_rt_str_free(value.label) };
    }

    #[test]
    fn a_struct_payload_is_read_out_independently_and_freed_with_its_box() {
        // SAFETY: every erased pointer uses `AggregateFixture`'s layout and every
        // owned handle is released exactly as many times as it is held.
        unsafe {
            let source = AggregateFixture {
                count: 7,
                label: str_handle("boxed"),
            };
            let value = kira_rt_enum_new_aggregate(
                3,
                std::ptr::from_ref(&source).cast::<u8>(),
                size_of::<AggregateFixture>(),
                Some(clone_fixture),
                Some(free_fixture),
            );
            let copy = kira_rt_enum_clone(value);
            assert_eq!(value, copy, "a copy is the same box");

            // A read *is* a copy, and the clone leaf is what makes it one: the
            // read takes a hold of the label, so it outlives the box below.
            let mut read = std::mem::MaybeUninit::<AggregateFixture>::uninit();
            kira_rt_enum_payload_aggregate(value, read.as_mut_ptr().cast::<u8>());
            let read = read.assume_init();
            assert_eq!(read.count, 7);
            kira_rt_enum_free(value);
            kira_rt_enum_free(copy);
            assert_eq!(crate::runtime::kira_rt_str_len(read.label), 5);
            kira_rt_str_free(read.label);
        }
    }

    /// The box is `#[repr(C)]`, so its layout is pinned here beside it. The
    /// share count is last, leaving the three fields before it where they were.
    #[test]
    fn the_enum_box_layout_is_pinned() {
        assert_eq!(size_of::<KiraEnum>(), 32);
        assert_eq!(align_of::<KiraEnum>(), 8);
        assert_eq!(size_of::<KEnum>(), size_of::<usize>());
        let box_ = KiraEnum {
            tag: 0,
            payload_kind: 0,
            payload: 0,
            shares: 1,
        };
        let base = std::ptr::from_ref(&box_).cast::<u8>();
        // SAFETY: every field belongs to `box_`, which outlives the reads.
        unsafe {
            assert_eq!(
                std::ptr::from_ref(&box_.tag).cast::<u8>().offset_from(base),
                0
            );
            assert_eq!(
                std::ptr::from_ref(&box_.payload_kind)
                    .cast::<u8>()
                    .offset_from(base),
                8
            );
            assert_eq!(
                std::ptr::from_ref(&box_.payload)
                    .cast::<u8>()
                    .offset_from(base),
                16
            );
            // The backend GEPs this field rather than calling a helper for it,
            // so where it sits is a contract with separately compiled code.
            assert_eq!(
                std::ptr::from_ref(&box_.shares)
                    .cast::<u8>()
                    .offset_from(base),
                isize::try_from(kira_runtime_abi::ENUM_BOX_SHARES_FIELD).expect("a small index")
                    * 8
            );
        }
    }

    /// A nested enum payload — what a `Result`-shaped `Error` variant carries —
    /// is held, not duplicated, and released exactly once with the last hold.
    ///
    /// Under Miri or ASan an over-release of the inner box would surface here
    /// as a use-after-free, and a missed one as a leak.
    #[test]
    fn a_nested_enum_payload_is_held_and_released_with_its_owner() {
        // SAFETY: every handle below is live and released once per hold.
        unsafe {
            // `Error(.MissingNode("boom"))`: an enum whose payload is an enum
            // whose payload is a string — two levels of nesting.
            let inner = kira_rt_enum_new(1, PAYLOAD_STR, str_handle("boom") as u64);
            let outer = kira_rt_enum_new(0, PAYLOAD_ENUM, inner as u64);

            let copy = kira_rt_enum_clone(outer);
            assert_eq!(outer, copy, "a copy is the same box");
            assert_eq!(kira_rt_enum_tag((*copy).payload as KEnum), 1);

            // A payload read is a hold of its own, so releasing the outer twice
            // over must leave it valid.
            let read = kira_rt_enum_payload(outer) as KEnum;
            assert_eq!(read as u64, (*outer).payload, "the read holds the same box");
            assert_eq!((*inner).shares, 2);
            kira_rt_enum_free(outer);
            kira_rt_enum_free(copy);
            assert_eq!(kira_rt_enum_tag(read), 1, "the read survives its source");
            kira_rt_enum_free(read);
        }
    }

    /// An unrecognized kind owns nothing rather than reinterpreting the word.
    #[test]
    fn an_unknown_payload_kind_is_treated_as_inert() {
        // SAFETY: the handle is live and freed exactly once; the payload word
        // is never dereferenced because the kind is not one that owns.
        unsafe {
            let value = kira_rt_enum_new(0, 99, 0xdead_beef);
            assert_eq!(kira_rt_enum_payload(value), 0xdead_beef);
            let copy = kira_rt_enum_clone(value);
            kira_rt_enum_free(value);
            kira_rt_enum_free(copy);
        }
    }

    #[test]
    fn a_null_handle_is_the_zero_value() {
        // SAFETY: a null handle is a valid tag-0 value; free is a no-op.
        unsafe {
            let empty: KEnum = std::ptr::null_mut();
            assert_eq!(kira_rt_enum_tag(empty), 0);
            assert!(kira_rt_enum_clone(empty).is_null());
            kira_rt_enum_free(empty);
        }
    }

    /// A payload-less variant is the handle, so constructing one allocates
    /// nothing and reading it back costs a shift.
    #[test]
    fn a_payload_less_variant_lives_in_its_handle() {
        for tag in [0_i64, 1, 7, 1024] {
            let value = inline_handle(tag);
            assert!(is_inline(value));
            // SAFETY: an inline handle is not a pointer and is never read as one.
            assert_eq!(unsafe { kira_rt_enum_tag(value) }, tag);
            // SAFETY: same handle; an inline one has no payload.
            assert_eq!(unsafe { kira_rt_enum_payload(value) }, 0);
        }
    }

    /// Copying one is identity and releasing one is nothing, which is what
    /// makes it free to read in a loop.
    #[test]
    fn an_inline_variant_owns_nothing() {
        let value = inline_handle(3);
        // SAFETY: an inline handle owns no allocation.
        let copy = unsafe { kira_rt_enum_clone(value) };
        assert_eq!(copy, value);
        // SAFETY: releasing an inline handle reclaims nothing, twice over.
        unsafe {
            kira_rt_enum_free(value);
            kira_rt_enum_free(copy);
        }
        // SAFETY: still readable, because nothing was ever freed.
        assert_eq!(unsafe { kira_rt_enum_tag(value) }, 3);
    }

    /// A boxed enum comes from the allocator word-aligned, so it never looks
    /// inline — the bit that tells them apart is only ever set deliberately.
    #[test]
    fn a_boxed_enum_is_never_mistaken_for_an_inline_one() {
        let boxed = kira_rt_enum_new(9, PAYLOAD_INERT, 42);
        assert!(!is_inline(boxed));
        // SAFETY: the handle is live.
        assert_eq!(unsafe { kira_rt_enum_tag(boxed) }, 9);
        // SAFETY: the handle is live and freed exactly once.
        unsafe { kira_rt_enum_free(boxed) };
    }
}
