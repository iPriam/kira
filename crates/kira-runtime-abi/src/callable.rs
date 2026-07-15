//! Native-closure pointer tagging (ABI bit tricks shared with generated code).
//!
//! Ported from kira-zig `kira_runtime_abi/src/callable.zig`.

/// High bit of `usize`, set on pointers that reference a native closure block
/// (Zig `native_closure_tag_bit`).
pub const NATIVE_CLOSURE_TAG_BIT: usize = 1 << (usize::BITS - 1);

/// Mask clearing [`NATIVE_CLOSURE_TAG_BIT`] (Zig `native_closure_pointer_mask`).
pub const NATIVE_CLOSURE_POINTER_MASK: usize = !NATIVE_CLOSURE_TAG_BIT;

/// Tags a raw pointer as a native closure pointer (Zig `tagNativeClosurePointer`).
pub const fn tag_native_closure_pointer(ptr: usize) -> usize {
    ptr | NATIVE_CLOSURE_TAG_BIT
}

/// Removes the native-closure tag (Zig `untagNativeClosurePointer`).
pub const fn untag_native_closure_pointer(ptr: usize) -> usize {
    ptr & NATIVE_CLOSURE_POINTER_MASK
}

/// True when the pointer carries the native-closure tag (Zig `isTaggedNativeClosurePointer`).
pub const fn is_tagged_native_closure_pointer(ptr: usize) -> bool {
    (ptr & NATIVE_CLOSURE_TAG_BIT) != 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_closure_pointer_tagging_is_reversible() {
        let raw: usize = 0x1234_5678;
        let tagged = tag_native_closure_pointer(raw);
        assert!(is_tagged_native_closure_pointer(tagged));
        assert_eq!(raw, untag_native_closure_pointer(tagged));
    }
}
