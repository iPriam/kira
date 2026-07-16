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

use kira_runtime_abi::{BridgeData, BridgeValue, NativeArg, NativeResult};

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
}

/// Lowers borrowed VM arguments into the array a trampoline reads.
///
/// Every string becomes a fresh handle out of the library's allocator. The
/// caller must **not** free them: the callee does, at its return.
pub fn lower_args(library: &NativeLibrary, args: &[NativeArg<'_>]) -> Vec<BridgeValue> {
    args.iter()
        .map(|argument| {
            let data = match *argument {
                NativeArg::Void => BridgeData::Void,
                NativeArg::Int(value) => BridgeData::Int(value),
                NativeArg::Float(value) => BridgeData::Float(value),
                NativeArg::Bool(value) => BridgeData::Bool(value),
                NativeArg::Str(text) => BridgeData::String(library.new_string(text)),
            };
            BridgeValue::encode(data)
        })
        .collect()
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
    }
}
