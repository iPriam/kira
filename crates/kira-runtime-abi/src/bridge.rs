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

    /// An array: a type a manifest can *describe* but this seam cannot carry
    /// yet.
    ///
    /// The tag exists for the same reason [`BridgeValueTag::STRUCT`] does — a
    /// manifest has a row for every function in the program, including the many
    /// that never cross — but the reason it does not travel is different, and
    /// the difference is worth stating.
    ///
    /// A struct **may not** cross: it does not fit one tag and one word, and
    /// deciding how it would is a language decision nobody has made.
    ///
    /// An array **should** cross — the language allows it — and does not yet
    /// only because the ownership question at the boundary is unanswered: who
    /// frees the elements, and what it means for the VM's heap accounting if a
    /// native function grows the array it was handed. A wrong answer there is a
    /// double free or a leak at the boundary, not a bad print, so it is refused
    /// until the answer is designed rather than guessed at.
    ///
    /// So: this tag names a type and never travels. Every marshalling path
    /// rejects it, and the backend refuses to emit a crossing whose signature
    /// mentions one.
    pub const ARRAY: BridgeValueTag = BridgeValueTag(6);

    /// An enum: a type a manifest can *describe* but this seam cannot carry.
    ///
    /// Exists for the same reason [`BridgeValueTag::STRUCT`] does — a manifest
    /// has a row for every function, and a `@Runtime` one may merely mention an
    /// enum in its signature — and it does not travel on the same grounds: an
    /// enum is a tagged value plus a payload, which does not fit one tag and one
    /// word, and how it would is a language decision nobody has made. Every
    /// marshalling path rejects it; this tag only names the type in a manifest.
    pub const ENUM: BridgeValueTag = BridgeValueTag(7);

    /// An opaque handle to an object one side owns and the other only names.
    ///
    /// Unlike [`BridgeValueTag::STRUCT`], [`BridgeValueTag::ARRAY`], and
    /// [`BridgeValueTag::ENUM`], this tag **travels**: it is what an `@Export`
    /// class's instances cross a library boundary as. The payload is one opaque
    /// word whose meaning belongs entirely to the side that produced it — a
    /// rooted-heap id when a VM-engine library made it, pointer bits when a
    /// native-engine one did. The receiving side never dereferences it, never
    /// interprets it, and never invents one: a handle it did not receive is a
    /// handle it cannot name.
    ///
    /// That is also why a handle fits where a struct does not. A struct would
    /// have to carry its fields across, and who frees the strings inside them is
    /// undesigned. A handle carries nothing: the object stays where it was
    /// allocated, and exactly one generated destructor frees it.
    ///
    /// Appended, never renumbered. The number is shared with the opposite
    /// direction (a Rust crate consumed *from* Kira), which needs the same tag
    /// for the same reason; it is defined here once so the two can never
    /// disagree.
    pub const HANDLE: BridgeValueTag = BridgeValueTag(8);

    /// An opaque target-width pointer word used only by foreign calls.
    ///
    /// The payload carries the pointer bits zero-extended to 64 bits. Kira never
    /// dereferences, performs arithmetic on, or frees the pointer.
    pub const RAW_PTR: BridgeValueTag = BridgeValueTag(9);

    /// A C-layout aggregate used only by foreign calls, carried by pointer.
    ///
    /// This tag **travels**, and it is the one tag whose payload is a pointer to
    /// storage rather than the value itself: the payload addresses the
    /// aggregate's `sizeof` bytes in the target's C layout. That is the whole
    /// reason it can exist where [`BridgeValueTag::STRUCT`] cannot. A Kira
    /// struct crossing the *native* seam raises an ownership question — who
    /// frees the strings inside — that has no answer yet. A C-layout aggregate
    /// raises none: its members are fixed-width scalars and nested aggregates
    /// of the same kind, so the bytes are the whole value, nothing inside them
    /// is owned, and the buffer belongs to the caller for exactly one call.
    ///
    /// The pointed-to bytes are read-only for an argument. For a result the
    /// caller presents the buffer and the adapter writes it; ownership never
    /// moves.
    pub const AGGREGATE: BridgeValueTag = BridgeValueTag(10);
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
    /// An opaque handle to an object the producing side owns.
    ///
    /// The word is meaningful only to whoever made it (see
    /// [`BridgeValueTag::HANDLE`]); nothing here reads it, and a receiver that
    /// cannot resolve it reports that rather than guessing.
    Handle(u64),
    /// An opaque target-width pointer word.
    ///
    /// Every bit pattern, including null, is data. The receiving side never
    /// dereferences or frees it.
    RawPtr(u64),
}

impl BridgeValue {
    /// The unit value.
    pub const VOID: BridgeValue = BridgeValue {
        tag: BridgeValueTag::VOID,
        reserved: [0; 7],
        payload: 0,
    };

    /// Builds a value from a tag and a payload, with the reserved bytes zeroed.
    ///
    /// The unchecked door, for a caller that already knows the tag it means:
    /// generated wrapper code packing one argument whose type it was generated
    /// from. [`BridgeValue::encode`] is the door for a caller holding *data*,
    /// and is the one to prefer — it cannot pair a tag with a payload the tag
    /// does not describe.
    ///
    /// The reserved bytes are zeroed here rather than left to the caller,
    /// because "must be zero" written on a public field is a rule somebody
    /// eventually does not read.
    pub fn new(tag: BridgeValueTag, payload: u64) -> BridgeValue {
        BridgeValue {
            tag,
            reserved: [0; 7],
            payload,
        }
    }

    /// Builds a value from decoded data.
    pub fn encode(data: BridgeData) -> BridgeValue {
        let (tag, payload) = match data {
            BridgeData::Void => (BridgeValueTag::VOID, 0),
            BridgeData::Int(value) => (BridgeValueTag::INT, value as u64),
            BridgeData::Float(value) => (BridgeValueTag::FLOAT, value.to_bits()),
            BridgeData::Bool(value) => (BridgeValueTag::BOOL, u64::from(value)),
            BridgeData::String(handle) => (BridgeValueTag::STRING, handle),
            BridgeData::Handle(handle) => (BridgeValueTag::HANDLE, handle),
            BridgeData::RawPtr(pointer) => (BridgeValueTag::RAW_PTR, pointer),
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
            // A handle's payload is opaque, so every bit pattern decodes: this
            // side is not the one that can tell a live handle from a stale one,
            // and the side that can rejects an unknown one by name.
            BridgeValueTag::HANDLE => BridgeData::Handle(self.payload),
            // Pointer bits are opaque and null is valid. Width checking belongs
            // to the native caller, which knows the target pointer width.
            BridgeValueTag::RAW_PTR => BridgeData::RawPtr(self.payload),
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
            BridgeData::Handle(0),
            BridgeData::Handle(1),
            BridgeData::Handle(u64::MAX),
            BridgeData::RawPtr(0),
            BridgeData::RawPtr(0x0123_4567_89ab_cdef),
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

    /// The tag numbers are the wire contract, so they are spelled out rather
    /// than only round-tripped: appending is safe, renumbering is not, and a
    /// renumbering must fail here instead of silently reinterpreting artifacts
    /// that already exist.
    #[test]
    fn the_tag_bytes_are_pinned() {
        assert_eq!(BridgeValueTag::VOID.0, 0);
        assert_eq!(BridgeValueTag::INT.0, 1);
        assert_eq!(BridgeValueTag::FLOAT.0, 2);
        assert_eq!(BridgeValueTag::BOOL.0, 3);
        assert_eq!(BridgeValueTag::STRING.0, 4);
        assert_eq!(BridgeValueTag::STRUCT.0, 5);
        assert_eq!(BridgeValueTag::ARRAY.0, 6);
        assert_eq!(BridgeValueTag::ENUM.0, 7);
        assert_eq!(BridgeValueTag::HANDLE.0, 8);
        assert_eq!(BridgeValueTag::RAW_PTR.0, 9);
    }

    /// A handle is written as tag 8 with the producer's word in the payload,
    /// unaltered, and nothing in the reserved bytes.
    #[test]
    fn a_handle_encodes_as_tag_eight_with_the_payload_untouched() {
        let encoded = BridgeValue::encode(BridgeData::Handle(0x0123_4567_89ab_cdef));
        assert_eq!(encoded.tag, BridgeValueTag::HANDLE);
        assert_eq!(encoded.tag.0, 8);
        assert_eq!(encoded.payload, 0x0123_4567_89ab_cdef);
        assert_eq!(encoded.reserved, [0; 7]);
    }

    /// Unlike the never-travels tags, a handle *does* cross, so every payload
    /// decodes here. Whether the word names a live object is a question only
    /// its producer can answer, and it answers by name rather than by guess.
    #[test]
    fn every_handle_payload_decodes_because_only_its_producer_can_judge_it() {
        for payload in [0, 1, u64::MAX] {
            let value = BridgeValue {
                tag: BridgeValueTag::HANDLE,
                reserved: [0; 7],
                payload,
            };
            assert_eq!(value.decode(), Some(BridgeData::Handle(payload)));
        }
    }

    /// Raw pointers append after handles and preserve every payload bit.
    #[test]
    fn a_raw_pointer_encodes_as_tag_nine() {
        let value = BridgeValue::encode(BridgeData::RawPtr(0x0123_4567_89ab_cdef));
        assert_eq!(value.tag, BridgeValueTag::RAW_PTR);
        assert_eq!(value.tag.0, 9);
        assert_eq!(value.payload, 0x0123_4567_89ab_cdef);
        assert_eq!(value.decode(), Some(BridgeData::RawPtr(value.payload)));
    }

    /// The tag after `RAW_PTR` remains unknown and must be rejected.
    #[test]
    fn tag_ten_is_rejected() {
        let value = BridgeValue {
            tag: BridgeValueTag(10),
            reserved: [0; 7],
            payload: 1,
        };
        assert_eq!(value.decode(), None);
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
