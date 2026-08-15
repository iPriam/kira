//! Marshalling across the seam, and who frees what.
//!
//! Both directions speak [`BridgeValue`] on the wire and the VM's safe
//! vocabulary ([`NativeArg`]/[`NativeResult`]) on this side. The tricky half is
//! not the encoding — it is ownership, which no compiler checks here and which
//! the generated code fixes rather than negotiates. The rules, each read out of
//! the codegen rather than out of the manifest:
//!
//! - **A native callee frees its string arguments.** `emit_return` frees every
//!   `String` local, and parameters occupy the leading locals. So
//!   [`lower_args`] builds a fresh handle per string and does *not* free it: the
//!   callee did.
//! - **A trampoline's result belongs to the host.** So [`lift_result`] copies
//!   the bytes out and frees the handle.
//! - **Args to a runtime call are transferred too.** `lower_runtime_call` writes
//!   handles into the array and never frees them after the call, so
//!   [`take_args`] takes ownership: read the bytes, free the handle.
//! - **A result the invoker returns must be a fresh handle**, which the native
//!   caller frees as an ordinary expression value. So [`lower_result`] allocates
//!   one out of the library's own allocator.

use std::ffi::c_void;

use kira_runtime_abi::{
    BridgeData, BridgeValue, BridgeValueTag, NativeArg, NativeResult, NativeStateError,
    NativeStateValue,
};

use crate::library::NativeLibrary;

/// Why a value that crossed the seam could not be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MarshalError {
    /// The value carries a tag this build does not know.
    #[error("value {index} carries tag {tag}, which this runtime does not know")]
    UnknownTag {
        /// Which value, by position.
        index: usize,
        /// The tag byte that named nothing.
        tag: u8,
    },
    /// The value is a string whose bytes are not UTF-8.
    #[error("value {index} is a string whose bytes are not valid UTF-8")]
    NotUtf8 {
        /// Which value, by position.
        index: usize,
    },
    /// A struct, array or enum value could not be built as a node tree.
    ///
    /// The tree is allocated out of the loaded native half, so this is that
    /// side refusing — a shape it has no node for, or an allocation it could
    /// not make. Reported by position, because a call with several aggregates
    /// otherwise says only that one of them failed.
    #[error("value {index} could not cross as a value tree: {reason}")]
    Aggregate {
        /// Which value, by position.
        index: usize,
        /// What the native half said.
        reason: NativeStateError,
    },
}

/// Lowers borrowed VM arguments into the array a trampoline reads.
///
/// Every string becomes a fresh handle out of the library's allocator. The
/// caller must **not** free them: the callee does, at its return.
pub fn lower_args(
    library: &NativeLibrary,
    args: &[NativeArg<'_>],
) -> Result<Vec<BridgeValue>, MarshalError> {
    args.iter()
        .enumerate()
        .map(|(index, argument)| {
            let data = match *argument {
                NativeArg::Void => BridgeData::Void,
                NativeArg::Int(value) => BridgeData::Int(value),
                NativeArg::Float(value) => BridgeData::Float(value),
                NativeArg::Bool(value) => BridgeData::Bool(value),
                NativeArg::Str(text) => BridgeData::String(library.new_string(text)),
                // A handle is one opaque word and copies like a scalar: there
                // is nothing to allocate and nothing for the callee to free.
                NativeArg::Handle(handle) => BridgeData::Handle(handle),
                // A raw pointer is likewise one opaque word that copies like a
                // scalar; Kira never dereferences or frees it.
                NativeArg::RawPtr(pointer) => BridgeData::RawPtr(pointer),
                // A payload-less enum is its variant tag: one word that copies
                // like a scalar, with nothing allocated here and nothing for
                // the callee to free.
                NativeArg::Enum(tag) => BridgeData::Enum(tag),
                // The tree is built in the native half's own allocator and
                // handed over; the callee's decode is what frees it. The VM
                // keeps the value this was copied from, exactly as it keeps
                // the string behind a `Str` argument.
                NativeArg::Aggregate(tree) => {
                    // SAFETY: every node is allocated by this library and
                    // consumed by the trampoline it is handed to.
                    let node = unsafe { library.encode_state_value(tree) }
                        .map_err(|reason| MarshalError::Aggregate { index, reason })?;
                    if matches!(tree, NativeStateValue::Any { .. }) {
                        BridgeData::Any(node as u64)
                    } else {
                        BridgeData::Node(node as u64)
                    }
                }
            };
            Ok(BridgeValue::encode(data))
        })
        .collect()
}

/// Lifts the final value of every parameter the callee wrote through.
///
/// A trampoline packs those back into the slots they arrived in, so this reads
/// the argument array *after* the call rather than anything the call returned.
/// Which slots to read comes from the library's own record of the manifest, so
/// the reader and the generated writer are working from one signature.
///
/// # Safety
/// `args` must be the array one of `library`'s trampolines was just called
/// with, and the values in the written-through slots must not have been lifted
/// or freed already.
pub unsafe fn lift_writebacks(
    library: &NativeLibrary,
    function_id: u32,
    args: &[BridgeValue],
) -> Result<Vec<(u32, NativeResult)>, MarshalError> {
    let mut writebacks = Vec::new();
    for (slot, mutable) in library.mutable_params(function_id).iter().enumerate() {
        if !mutable {
            continue;
        }
        let value = args.get(slot).copied().ok_or(MarshalError::UnknownTag {
            index: slot,
            tag: BridgeValueTag::VOID.0,
        })?;
        // SAFETY: the caller vouches that this is the array the trampoline
        // wrote and that the slot has not been lifted yet.
        writebacks.push((slot as u32, unsafe { lift_result(library, value) }?));
    }
    Ok(writebacks)
}

/// Lifts what a trampoline wrote into an owned result, freeing its string.
///
/// # Safety
/// `value` must be what one of `library`'s trampolines just wrote, and its
/// string handle (if any) must not have been freed.
pub unsafe fn lift_result(
    library: &NativeLibrary,
    value: BridgeValue,
) -> Result<NativeResult, MarshalError> {
    let data = value.decode().ok_or(MarshalError::UnknownTag {
        index: 0,
        tag: value.tag.0,
    })?;
    Ok(match data {
        BridgeData::Void => NativeResult::Void,
        BridgeData::Int(value) => NativeResult::Int(value),
        BridgeData::Float(value) => NativeResult::Float(value),
        BridgeData::Bool(value) => NativeResult::Bool(value),
        // The result is the host's now: copy the bytes out, then free.
        BridgeData::String(handle) => {
            // SAFETY: the caller vouches the handle is live and unfreed; this
            // consumes it exactly once.
            let text = unsafe { library.take_string(handle) };
            NativeResult::Str(text.map_err(|_| MarshalError::NotUtf8 { index: 0 })?)
        }
        // Carried through untouched: this layer cannot tell whether the word
        // names a live object, and the VM — which can — refuses a handle it has
        // no representation for, by name.
        BridgeData::Handle(handle) => NativeResult::Handle(handle),
        // An opaque pointer word, carried through with no allocation or free.
        BridgeData::RawPtr(pointer) => NativeResult::RawPtr(pointer),
        // Nothing was allocated for it, so nothing is freed: the tag is the
        // whole value and the VM rebuilds its own enum from it.
        BridgeData::Enum(tag) => NativeResult::Enum(tag),
        // The tree is the host's now: decoding copies it out and frees every
        // node, which is the one free the transfer owes.
        BridgeData::Node(node) => {
            // SAFETY: the caller vouches the node came from this library and
            // has not been freed; decoding consumes it exactly once.
            let tree = unsafe { library.decode_state_value(node as *mut c_void) }
                .map_err(|reason| MarshalError::Aggregate { index: 0, reason })?;
            NativeResult::Aggregate(tree)
        }
        BridgeData::Any(node) => {
            // SAFETY: the caller vouches the node came from this library and
            // has not been freed; decoding consumes it exactly once.
            let tree = unsafe { library.decode_state_value(node as *mut c_void) }
                .map_err(|reason| MarshalError::Aggregate { index: 0, reason })?;
            NativeResult::Aggregate(tree)
        }
    })
}

/// One argument the invoker has taken ownership of.
///
/// A [`NativeArg`] borrows its string, so the owned text has to live somewhere
/// while the call runs. This is that somewhere.
#[derive(Debug, Clone, PartialEq)]
pub enum OwnedArg {
    /// The unit value.
    Void,
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// An owned string, copied out of the handle native code transferred.
    Str(String),
    /// An opaque handle, copied like a scalar — nothing was transferred to own.
    Handle(u64),
    /// An opaque target-width pointer word, copied like a scalar.
    RawPtr(u64),
    /// A payload-less enum's variant tag, copied like a scalar.
    ///
    /// Owns nothing, so unlike [`OwnedArg::Str`] there was never a handle to
    /// take: the tag is the whole value.
    Enum(i64),
    /// A struct, array or payload-carrying enum, as an owned value tree.
    ///
    /// Owned like [`OwnedArg::Str`]: the tree was decoded out of the nodes
    /// native code transferred, and that copy is the invoker's.
    Aggregate(NativeStateValue),
}

impl OwnedArg {
    /// Borrows this argument as the VM's own vocabulary.
    pub fn borrow(&self) -> NativeArg<'_> {
        match self {
            OwnedArg::Void => NativeArg::Void,
            OwnedArg::Int(value) => NativeArg::Int(*value),
            OwnedArg::Float(value) => NativeArg::Float(*value),
            OwnedArg::Bool(value) => NativeArg::Bool(*value),
            OwnedArg::Str(text) => NativeArg::Str(text),
            OwnedArg::Handle(handle) => NativeArg::Handle(*handle),
            OwnedArg::RawPtr(pointer) => NativeArg::RawPtr(*pointer),
            OwnedArg::Enum(tag) => NativeArg::Enum(*tag),
            OwnedArg::Aggregate(tree) => NativeArg::Aggregate(tree),
        }
    }
}

/// Takes ownership of the arguments native code handed the invoker.
///
/// Every string handle in `values` is freed here, including when another value
/// in the same array is rejected — a caller that bails on the first bad tag
/// would leak the handles it had already been given.
///
/// # Safety
/// Every string handle in `values` must be live, unfreed, and transferred to
/// this call.
pub unsafe fn take_args(
    library: &NativeLibrary,
    values: &[BridgeValue],
) -> Result<Vec<OwnedArg>, MarshalError> {
    let mut owned = Vec::with_capacity(values.len());
    let mut failure = None;

    for (index, value) in values.iter().enumerate() {
        let Some(data) = value.decode() else {
            failure.get_or_insert(MarshalError::UnknownTag {
                index,
                tag: value.tag.0,
            });
            continue;
        };
        let argument = match data {
            BridgeData::Void => OwnedArg::Void,
            BridgeData::Int(value) => OwnedArg::Int(value),
            BridgeData::Float(value) => OwnedArg::Float(value),
            BridgeData::Bool(value) => OwnedArg::Bool(value),
            // SAFETY: the caller vouches every handle is live and transferred;
            // each is consumed exactly once, here.
            BridgeData::String(handle) => match unsafe { library.take_string(handle) } {
                Ok(text) => OwnedArg::Str(text),
                Err(_) => {
                    failure.get_or_insert(MarshalError::NotUtf8 { index });
                    continue;
                }
            },
            // Nothing to take: a handle is a word, and the object behind it
            // belongs to whoever minted it for as long as it lives.
            BridgeData::Handle(handle) => OwnedArg::Handle(handle),
            // An opaque pointer word: nothing to take ownership of.
            BridgeData::RawPtr(pointer) => OwnedArg::RawPtr(pointer),
            // Nothing to take: the tag is the whole value.
            BridgeData::Enum(tag) => OwnedArg::Enum(tag),
            // Taken like a string handle: decoding consumes the tree.
            BridgeData::Node(node) => {
                // SAFETY: the caller vouches every node is live and
                // transferred; each is consumed exactly once, here.
                match unsafe { library.decode_state_value(node as *mut c_void) } {
                    Ok(tree) => OwnedArg::Aggregate(tree),
                    Err(reason) => {
                        failure.get_or_insert(MarshalError::Aggregate { index, reason });
                        continue;
                    }
                }
            }
            BridgeData::Any(node) => {
                // SAFETY: the caller vouches every node is live and
                // transferred; decoding consumes it exactly once.
                match unsafe { library.decode_state_value(node as *mut c_void) } {
                    Ok(tree) => OwnedArg::Aggregate(tree),
                    Err(reason) => {
                        failure.get_or_insert(MarshalError::Aggregate { index, reason });
                        continue;
                    }
                }
            }
        };
        owned.push(argument);
    }

    match failure {
        Some(error) => Err(error),
        None => Ok(owned),
    }
}

/// Lowers what a runtime function returned into the value native code frees.
///
/// A returned string becomes a fresh handle from the library's own allocator:
/// the native caller treats it as an ordinary expression value and frees it.
pub fn lower_result(library: &NativeLibrary, result: NativeResult) -> BridgeValue {
    let data = match result {
        NativeResult::Void => BridgeData::Void,
        NativeResult::Int(value) => BridgeData::Int(value),
        NativeResult::Float(value) => BridgeData::Float(value),
        NativeResult::Bool(value) => BridgeData::Bool(value),
        NativeResult::Str(text) => BridgeData::String(library.new_string(&text)),
        NativeResult::Handle(handle) => BridgeData::Handle(handle),
        NativeResult::RawPtr(pointer) => BridgeData::RawPtr(pointer),
        NativeResult::Enum(tag) => BridgeData::Enum(tag),
        NativeResult::Aggregate(tree) => {
            // SAFETY: the node is allocated by this library and consumed by
            // the native caller that receives the result.
            match unsafe { library.encode_state_value(&tree) } {
                Ok(node) if matches!(tree, NativeStateValue::Any { .. }) => {
                    BridgeData::Any(node as u64)
                }
                Ok(node) => BridgeData::Node(node as u64),
                // A tree the native half cannot build is reported as the unit
                // value: this path has no error channel, and a wrong value
                // would be worse than an empty one.
                Err(_) => BridgeData::Void,
            }
        }
    };
    BridgeValue::encode(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_owned_arg_borrows_back_as_the_vms_vocabulary() {
        assert_eq!(OwnedArg::Int(7).borrow(), NativeArg::Int(7));
        assert_eq!(OwnedArg::Bool(true).borrow(), NativeArg::Bool(true));
        assert_eq!(OwnedArg::Void.borrow(), NativeArg::Void);
        let text = OwnedArg::Str("hi".to_owned());
        assert_eq!(text.borrow(), NativeArg::Str("hi"));
        assert_eq!(OwnedArg::Handle(9).borrow(), NativeArg::Handle(9));
    }

    /// A handle round-trips through the wire encoding with its payload intact
    /// and no allocation on either side — the property that makes it copy like
    /// a scalar rather than move like a string.
    #[test]
    fn a_handle_survives_the_wire_encoding_unaltered() {
        let encoded = BridgeValue::encode(BridgeData::Handle(u64::MAX));
        assert_eq!(encoded.decode(), Some(BridgeData::Handle(u64::MAX)));
    }
}
