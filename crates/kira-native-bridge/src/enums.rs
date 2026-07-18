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
//!   tag       the variant's discriminant, the value `==` compares
//!   owns_str  1 when `payload` is an owned `KStr`, 0 when it is inert bits
//!   payload   the variant's single payload, type-erased into one word
//! ```
//!
//! # Why the payload is one word plus a flag, not a type
//!
//! The box is generic over the variant's payload type. A scalar (`Int`,
//! `Float`, `Bool`) fits one word directly — the backend passes its bits and
//! this code copies them, owning nothing. A `String` payload is an owned `KStr`
//! handle, which is also one word, but it must be cloned when the enum is
//! cloned and freed when the enum is freed. The `owns_str` flag is what lets
//! one clone/free pair serve both without the box carrying the payload's type:
//! the flag says whether that word is a handle to reclaim.
//!
//! A struct/enum/array payload is refused at the declaration (`KSEM118`), so
//! the one-word slot never has to carry an aggregate.
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

use crate::runtime::{KStr, kira_rt_str_clone, kira_rt_str_free};

/// A Kira enum at the native ABI: an opaque owned handle.
pub type KEnum = *mut KiraEnum;

/// The heap box behind a [`KEnum`].
///
/// `#[repr(C)]` because the backend, compiled separately, references this
/// layout only through the `kira_rt_enum_*` helpers — but keeping it `repr(C)`
/// makes the intent explicit and the layout stable.
#[repr(C)]
pub struct KiraEnum {
    /// The variant's discriminant.
    tag: i64,
    /// 1 when `payload` is an owned `KStr` to clone/free; 0 otherwise.
    owns_str: i64,
    /// The variant's single payload, type-erased into one word.
    payload: u64,
}

/// Boxes a fresh enum value.
///
/// `owns_str` is 1 when `payload` is a `KStr` the box takes ownership of, and 0
/// when it holds inert scalar bits (or no payload — a payload-less variant
/// passes 0/0).
#[unsafe(no_mangle)]
pub extern "C" fn kira_rt_enum_new(tag: i64, owns_str: i64, payload: u64) -> KEnum {
    Box::into_raw(Box::new(KiraEnum {
        tag,
        owns_str,
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
/// This is what a `match` arm's binding reads. A `String` payload is cloned, so
/// the returned handle is the caller's to free and the box still owns its own —
/// the same affine discipline [`kira_rt_enum_clone`] follows. A scalar payload
/// is returned by bits and owns nothing. A null handle reads as 0, mirroring
/// [`kira_rt_enum_tag`].
///
/// The caller knows from the variant's declared payload type whether the word
/// is a `KStr` to free, which is why the flag does not have to come back with
/// it.
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
    if source.owns_str != 0 {
        // SAFETY: `owns_str` promises `payload` is a live `KStr`; cloning it
        // reads it and leaves it in place.
        return unsafe { kira_rt_str_clone(source.payload as KStr) } as u64;
    }
    source.payload
}

/// Produces an independent copy of an enum (clone-on-read for locals).
///
/// A `String` payload is cloned so the copy shares no storage with the source;
/// a scalar payload is copied by bits. A null handle clones to null.
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
    let payload = if source.owns_str != 0 {
        // SAFETY: `owns_str` promises `payload` is a live `KStr`; cloning it
        // reads it and leaves it in place.
        let cloned = unsafe { kira_rt_str_clone(source.payload as KStr) };
        cloned as u64
    } else {
        source.payload
    };
    Box::into_raw(Box::new(KiraEnum {
        tag: source.tag,
        owns_str: source.owns_str,
        payload,
    }))
}

/// Frees an enum, releasing an owned `String` payload. A null handle is a
/// no-op.
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
    if boxed.owns_str != 0 {
        // SAFETY: `owns_str` promises `payload` is a live `KStr`, freed here
        // exactly once as the box is reclaimed.
        unsafe { kira_rt_str_free(boxed.payload as KStr) };
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
            let value = kira_rt_enum_new(2, 0, 42);
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
            let value = kira_rt_enum_new(0, 1, str_handle("payload") as u64);
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
            let value = kira_rt_enum_new(1, 1, str_handle("bound") as u64);
            let read = kira_rt_enum_payload(value) as KStr;
            assert_ne!(
                read as u64,
                (*value).payload,
                "the read owns its own string"
            );
            kira_rt_enum_free(value);
            kira_rt_str_free(read);

            let scalar = kira_rt_enum_new(0, 0, 77);
            assert_eq!(kira_rt_enum_payload(scalar), 77);
            kira_rt_enum_free(scalar);
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
