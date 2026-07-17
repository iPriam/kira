//! The value ABI for calls that cross the runtime/native boundary.
//!
//! A [`BridgeValue`] is how one Kira value is handed between the VM and native
//! code in either direction. It is deliberately a *flat, 16-byte, union-free*
//! struct: a tag plus one 64-bit payload word.
//!
//! - Union-free, because a union's active field is only knowable from the tag,
//!   and native code on the other side of this boundary is free to write a tag
//!   this crate has never heard of. One payload word that each type encodes
//!   itself into is checkable; a union is not.
//! - 16 bytes with an explicit reserved gap, so the payload is 8-byte aligned
//!   on every host without the compiler choosing padding for us.
//!
//! # Wire contract
//!
//! This layout is shared with generated native code and is append-only: never
//! renumber a tag, never change the size, never repurpose the reserved bytes
//! without a new tag. The layout tests in this file are the guard.

/// How a [`BridgeValue`]'s payload should be read.
///
/// A plain byte, not a Rust `enum`: native code writes this field, and a Rust
/// `enum` holding a discriminant it never declared would be undefined
/// behaviour. Unknown tags decode to `None` rather than trapping.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BridgeValueTag(pub u8);

impl BridgeValueTag {
    /// The unit value; the payload is unused.
    pub const VOID: BridgeValueTag = BridgeValueTag(0);
    /// A 64-bit signed integer, payload as two's-complement bits.
    pub const INT: BridgeValueTag = BridgeValueTag(1);
    /// A 64-bit float, payload as IEEE-754 bits.
    pub const FLOAT: BridgeValueTag = BridgeValueTag(2);
    /// A boolean, payload 0 or 1.
    pub const BOOL: BridgeValueTag = BridgeValueTag(3);
    /// A string, payload as an owned native string handle.
    pub const STRING: BridgeValueTag = BridgeValueTag(4);
    /// A struct: a type a manifest can *describe* but the seam cannot carry.
    ///
    /// A struct does not fit a [`BridgeValue`] — one tag and one word of
    /// payload — and passing one would need an ABI decision (by value or by
    /// pointer, and who frees the strings inside) that has not been made. The
    /// tag exists anyway because a manifest describes every function in the
    /// program, including the many that never cross: a `@Runtime` function
    /// taking a struct and called only from other `@Runtime` code is an
    /// ordinary program, and its row has to say what its parameters are.
    ///
    /// So this tag names a type, and never travels: no [`BridgeValue`] is ever
    /// built with it, and every marshalling path rejects it. What enforces that
    /// is the backend, which refuses to emit a crossing whose signature
    /// mentions a struct.
    pub const STRUCT: BridgeValueTag = BridgeValueTag(5);
}

/// One Kira value crossing the runtime/native boundary.
///
/// The payload's meaning is fixed by [`BridgeValue::tag`]; read it through
/// [`BridgeValue::decode`] rather than by hand, so an unknown tag from foreign
/// code cannot be mistaken for a value.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BridgeValue {
    /// How to read [`BridgeValue::payload`].
    pub tag: BridgeValueTag,
    /// Padding to align the payload; must be zero.
    ///
    /// Reserved for a future tag's flags. Zeroed on every value this crate
    /// builds so a later reader can rely on it.
    pub reserved: [u8; 7],
    /// The value's bits, interpreted per the tag.
    pub payload: u64,
}

/// A decoded [`BridgeValue`]: what the tag and payload mean together.
///
/// Deliberately separate from the wire struct — this is a closed, Kira-owned
/// Rust enum, safe to match on, produced only after the tag has been checked.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BridgeData {
    /// The unit value.
    Void,
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A boolean.
    Bool(bool),
    /// An owned native string handle. Null is the empty string.
    ///
    /// The handle is opaque here: this crate is in the VM's portable cone and
    /// never dereferences it. Only the native side owns and frees it.
    String(u64),
}

impl BridgeValue {
    /// The unit value.
    pub const VOID: BridgeValue = BridgeValue {
        tag: BridgeValueTag::VOID,
        reserved: [0; 7],
        payload: 0,
    };

    /// Builds a value from decoded data.
    pub fn encode(data: BridgeData) -> BridgeValue {
        let (tag, payload) = match data {
            BridgeData::Void => (BridgeValueTag::VOID, 0),
            BridgeData::Int(value) => (BridgeValueTag::INT, value as u64),
            BridgeData::Float(value) => (BridgeValueTag::FLOAT, value.to_bits()),
            BridgeData::Bool(value) => (BridgeValueTag::BOOL, u64::from(value)),
            BridgeData::String(handle) => (BridgeValueTag::STRING, handle),
        };
        BridgeValue {
            tag,
            reserved: [0; 7],
            payload,
        }
    }

    /// Reads a value, or `None` when its tag is one this build does not know.
    ///
    /// Foreign code writes these, so an unrecognized tag is data to reject,
    /// never a reason to panic.
    pub fn decode(self) -> Option<BridgeData> {
        Some(match self.tag {
            BridgeValueTag::VOID => BridgeData::Void,
            BridgeValueTag::INT => BridgeData::Int(self.payload as i64),
            BridgeValueTag::FLOAT => BridgeData::Float(f64::from_bits(self.payload)),
            // Any non-zero payload is `true`: native code is not obliged to
            // normalize a bool to exactly 1.
            BridgeValueTag::BOOL => BridgeData::Bool(self.payload != 0),
            BridgeValueTag::STRING => BridgeData::String(self.payload),
            _ => return None,
        })
    }
}

impl Default for BridgeValue {
    fn default() -> Self {
        BridgeValue::VOID
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The layout is a wire contract with generated native code; it changes
    /// only together with the code that emits it.
    #[test]
    fn layout_is_a_fixed_sixteen_bytes() {
        assert_eq!(size_of::<BridgeValue>(), 16);
        assert_eq!(align_of::<BridgeValue>(), 8);
        assert_eq!(size_of::<BridgeValueTag>(), 1);
        assert_eq!(std::mem::offset_of!(BridgeValue, tag), 0);
        assert_eq!(std::mem::offset_of!(BridgeValue, reserved), 1);
        assert_eq!(std::mem::offset_of!(BridgeValue, payload), 8);
    }

    #[test]
    fn every_value_round_trips() {
        for data in [
            BridgeData::Void,
            BridgeData::Int(0),
            BridgeData::Int(-9223372036854775808),
            BridgeData::Int(9223372036854775807),
            BridgeData::Float(3.5),
            BridgeData::Float(-0.0),
            BridgeData::Bool(true),
            BridgeData::Bool(false),
            BridgeData::String(0),
            BridgeData::String(0xdead_beef),
        ] {
            let encoded = BridgeValue::encode(data);
            assert_eq!(
                encoded.decode(),
                Some(data),
                "round trip failed for {data:?}"
            );
            assert_eq!(encoded.reserved, [0; 7], "reserved bytes must be zeroed");
        }
    }

    #[test]
    fn float_payloads_survive_bit_for_bit() {
        // Bits, not value: NaN never equals itself, but the boundary must not
        // alter the payload it was handed.
        let nan = BridgeValue::encode(BridgeData::Float(f64::NAN));
        let Some(BridgeData::Float(value)) = nan.decode() else {
            panic!("a float decodes as a float");
        };
        assert!(value.is_nan());
        assert_eq!(nan.payload, f64::NAN.to_bits());
    }

    #[test]
    fn an_unknown_tag_is_rejected_rather_than_guessed() {
        let foreign = BridgeValue {
            tag: BridgeValueTag(200),
            reserved: [0; 7],
            payload: 1,
        };
        assert_eq!(foreign.decode(), None);
    }

    #[test]
    fn a_struct_tag_names_a_type_but_never_decodes_as_a_value() {
        // `STRUCT` exists so a manifest can describe a function that never
        // crosses. It must never be readable as a value: if one ever appeared
        // on the wire, guessing at its payload would be guessing at ownership.
        let impossible = BridgeValue {
            tag: BridgeValueTag::STRUCT,
            reserved: [0; 7],
            payload: 0xdead_beef,
        };
        assert_eq!(impossible.decode(), None);
    }

    #[test]
    fn a_bool_from_native_code_need_not_be_normalized() {
        let sloppy = BridgeValue {
            tag: BridgeValueTag::BOOL,
            reserved: [0; 7],
            payload: 42,
        };
        assert_eq!(sloppy.decode(), Some(BridgeData::Bool(true)));
    }
}
