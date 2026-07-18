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
//! handle, and a *nested enum* payload is an owned [`KEnum`] handle; both are
//! one word too, but each must be cloned when the enum is cloned and freed when
//! it is freed. [`KiraEnum::payload_kind`] is what lets one clone/free pair
//! serve all three without the box carrying the payload's type: the kind says
//! whether that word is a handle to reclaim, and which kind of handle.
//!
//! A nested enum is what `Result`-shaped values are made of — `Error` carries
//! the failure enum — so `attempt`/`try`/`handle` is the construct that needs
//! [`PAYLOAD_ENUM`]. Recursion terminates because a payload's type is resolved
//! against types that already resolve, so a cycle is unrepresentable; the VM's
//! heap relies on the same argument.
//!
//! A struct or array payload is still refused at the declaration (`KSEM118`),
//! so the one-word slot never has to carry an aggregate.
//!
//! # Ownership
//!
//! Affine, mirroring the VM's heap: reading an enum clones it
//! ([`kira_rt_enum_clone`]), and a local leaving scope or being overwritten
//! frees it ([`kira_rt_enum_free`]). A well-formed program frees every
//! allocation exactly once — the guarantee the VM proves with its heap
//! accounting.
//!
//! Every symbol is `extern "C"` with a `kira_rt_` prefix and a fixed signature.
//! These names are a wire contract with the backend's lowering and are
//! append-only: never rename one or change a signature in place.

use kira_runtime_abi::EnumPayloadKind;

use crate::runtime::{KStr, kira_rt_str_clone, kira_rt_str_free};

/// A Kira enum at the native ABI: an opaque owned handle.
pub type KEnum = *mut KiraEnum;

/// Payload word is inert bits (a scalar, or no payload at all); owns nothing.
pub const PAYLOAD_INERT: i64 = EnumPayloadKind::INERT.as_i64();
/// Payload word is an owned [`KStr`] to clone and free with the box.
pub const PAYLOAD_STR: i64 = EnumPayloadKind::STR.as_i64();
/// Payload word is an owned [`KEnum`] to clone and free with the box.
pub const PAYLOAD_ENUM: i64 = EnumPayloadKind::ENUM.as_i64();

/// The heap box behind a [`KEnum`].
///
/// `#[repr(C)]` because the backend, compiled separately, references this
/// layout only through the `kira_rt_enum_*` helpers — but keeping it `repr(C)`
/// makes the intent explicit and the layout stable.
#[repr(C)]
pub struct KiraEnum {
    /// The variant's discriminant.
    tag: i64,
    /// What `payload` is: [`PAYLOAD_INERT`], [`PAYLOAD_STR`], or
    /// [`PAYLOAD_ENUM`].
    payload_kind: i64,
    /// The variant's single payload, type-erased into one word.
    payload: u64,
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
    Box::into_raw(Box::new(KiraEnum {
        tag,
        payload_kind,
        payload,
    }))
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
    if value.is_null() {
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
        _ => source.payload,
    }
}

/// Produces an independent copy of an enum (clone-on-read for locals).
///
/// A `String` or nested-enum payload is cloned so the copy shares no storage
/// with the source; a scalar payload is copied by bits. A null handle clones to
/// null. The clone is deep, matching the VM's `Heap::copy_value`.
///
/// # Safety
/// `value` must be null or a live handle; it is left untouched.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_clone(value: KEnum) -> KEnum {
    if value.is_null() {
        return std::ptr::null_mut();
    }
    // SAFETY: a non-null handle is a live `KiraEnum` that outlives this read.
    let source = unsafe { &*value };
    let payload = match source.payload_kind {
        // SAFETY: the kind promises `payload` is a live `KStr`; cloning it
        // reads it and leaves it in place.
        PAYLOAD_STR => (unsafe { kira_rt_str_clone(source.payload as KStr) }) as u64,
        // SAFETY: the kind promises `payload` is a live `KEnum`; cloning it
        // reads it and leaves it in place.
        PAYLOAD_ENUM => (unsafe { kira_rt_enum_clone(source.payload as KEnum) }) as u64,
        _ => source.payload,
    };
    Box::into_raw(Box::new(KiraEnum {
        tag: source.tag,
        payload_kind: source.payload_kind,
        payload,
    }))
}

/// Frees an enum, releasing an owned `String` or nested-enum payload. A null
/// handle is a no-op.
///
/// # Safety
/// `value` must be null or a live handle from this runtime, freed at most once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn kira_rt_enum_free(value: KEnum) {
    if value.is_null() {
        return;
    }
    // SAFETY: the handle came from `Box::into_raw`, and the caller's free-once
    // contract makes this the only reclaim of it.
    let boxed = unsafe { Box::from_raw(value) };
    match boxed.payload_kind {
        // SAFETY: the kind promises `payload` is a live `KStr`, freed here
        // exactly once as the box is reclaimed.
        PAYLOAD_STR => unsafe { kira_rt_str_free(boxed.payload as KStr) },
        // SAFETY: the kind promises `payload` is a live `KEnum`, freed here
        // exactly once as the box is reclaimed. Recursion is bounded by the
        // program's nesting depth, which is finite because a payload type
        // resolves against types that already resolve.
        PAYLOAD_ENUM => unsafe { kira_rt_enum_free(boxed.payload as KEnum) },
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
        // SAFETY: the handle is live and freed exactly once.
        unsafe {
            let value = kira_rt_enum_new(2, PAYLOAD_INERT, 42);
            assert_eq!(kira_rt_enum_tag(value), 2);
            let copy = kira_rt_enum_clone(value);
            assert_eq!(kira_rt_enum_tag(copy), 2);
            assert_ne!(value, copy, "a clone is an independent allocation");
            kira_rt_enum_free(value);
            kira_rt_enum_free(copy);
        }
    }

    #[test]
    fn a_string_payload_is_cloned_and_freed_independently() {
        // A clone must own its own string, so freeing the source leaves the
        // clone valid — the affine guarantee the VM proves with heap counters.
        // Under Miri or ASan a shared handle would surface here as a double
        // free; a leak would surface as a reported leak.
        // SAFETY: every handle below is live and freed exactly once.
        unsafe {
            let value = kira_rt_enum_new(0, PAYLOAD_STR, str_handle("payload") as u64);
            let copy = kira_rt_enum_clone(value);
            assert_ne!(
                (*value).payload,
                (*copy).payload,
                "the clone owns its own string"
            );
            kira_rt_enum_free(value);
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
            assert_ne!(
                read as u64,
                (*value).payload,
                "the read owns its own string"
            );
            kira_rt_enum_free(value);
            kira_rt_str_free(read);

            let scalar = kira_rt_enum_new(0, PAYLOAD_INERT, 77);
            assert_eq!(kira_rt_enum_payload(scalar), 77);
            kira_rt_enum_free(scalar);
        }
    }

    /// The box is `#[repr(C)]`, so its layout is pinned here beside it.
    #[test]
    fn the_enum_box_layout_is_pinned() {
        assert_eq!(size_of::<KiraEnum>(), 24);
        assert_eq!(align_of::<KiraEnum>(), 8);
        assert_eq!(size_of::<KEnum>(), size_of::<usize>());
    }

    /// A nested enum payload — what a `Result`-shaped `Error` variant carries —
    /// is cloned deeply and freed exactly once with its owner.
    ///
    /// Under Miri or ASan a shared inner handle would surface here as a double
    /// free, and a missed recursive free as a leak.
    #[test]
    fn a_nested_enum_payload_is_cloned_deeply_and_freed_with_its_owner() {
        // SAFETY: every handle below is live and freed exactly once.
        unsafe {
            // `Error(.MissingNode("boom"))`: an enum whose payload is an enum
            // whose payload is a string — two levels of recursion.
            let inner = kira_rt_enum_new(1, PAYLOAD_STR, str_handle("boom") as u64);
            let outer = kira_rt_enum_new(0, PAYLOAD_ENUM, inner as u64);

            let copy = kira_rt_enum_clone(outer);
            assert_ne!(
                (*outer).payload,
                (*copy).payload,
                "the clone owns its own nested enum"
            );
            assert_eq!(kira_rt_enum_tag((*copy).payload as KEnum), 1);

            // A payload read is owned: freeing the outer must leave it valid.
            let read = kira_rt_enum_payload(outer) as KEnum;
            assert_ne!(read as u64, (*outer).payload, "the read owns its own enum");
            kira_rt_enum_free(outer);
            assert_eq!(kira_rt_enum_tag(read), 1, "the read survives its source");
            kira_rt_enum_free(read);
            kira_rt_enum_free(copy);
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
}
