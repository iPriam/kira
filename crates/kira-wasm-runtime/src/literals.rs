//! The literal string pool: every constant string a module needs, laid out in
//! the data section.
//!
//! A literal is a string object like any other — a length word then its bytes —
//! so a `String` value can be a literal's address with no conversion at the use
//! site. Nothing ever mutates a string, and the heap never frees, so literals
//! are shared freely: `str_from_bool` hands back the same `"true"` every time
//! rather than allocating one.

use std::collections::BTreeMap;

use crate::layout;

/// Collects string constants and assigns each an address.
#[derive(Debug)]
pub struct Literals {
    /// Content to address, deduplicated. Ordered so a build is reproducible:
    /// the same program must produce the same bytes on every run.
    addresses: BTreeMap<String, u64>,
    bytes: Vec<u8>,
}

impl Default for Literals {
    fn default() -> Self {
        Self::new()
    }
}

impl Literals {
    /// Creates an empty pool.
    pub fn new() -> Self {
        Self {
            addresses: BTreeMap::new(),
            bytes: Vec::new(),
        }
    }

    /// Interns `text`, returning the address of its string object.
    ///
    /// Interning the same text twice yields the same address.
    pub fn intern(&mut self, text: &str) -> u64 {
        if let Some(address) = self.addresses.get(text) {
            return *address;
        }

        // Each object starts 4-byte aligned so its length word is a naturally
        // aligned load.
        while !self.bytes.len().is_multiple_of(4) {
            self.bytes.push(0);
        }
        let address = u64::from(layout::LITERALS) + self.bytes.len() as u64;
        self.bytes
            .extend_from_slice(&(text.len() as u32).to_le_bytes());
        self.bytes.extend_from_slice(text.as_bytes());
        self.addresses.insert(text.to_owned(), address);
        address
    }

    /// The pool's bytes, to be placed at [`layout::LITERALS`].
    pub fn data(&self) -> &[u8] {
        &self.bytes
    }

    /// The first address past the pool, aligned — where the heap begins.
    pub fn heap_base(&self) -> u64 {
        let end = u64::from(layout::LITERALS) + self.bytes.len() as u64;
        let align = u64::from(layout::ALIGN);
        end.div_ceil(align) * align
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_text_interns_to_one_address() {
        let mut pool = Literals::new();
        let first = pool.intern("hello");
        let second = pool.intern("hello");
        assert_eq!(first, second);
        assert_eq!(pool.data().len(), 4 + 5);
    }

    #[test]
    fn a_literal_is_a_length_word_then_its_bytes() {
        let mut pool = Literals::new();
        let address = pool.intern("hi");
        assert_eq!(address, u64::from(layout::LITERALS));
        assert_eq!(pool.data(), &[0x02, 0x00, 0x00, 0x00, b'h', b'i']);
    }

    #[test]
    fn every_literal_starts_four_byte_aligned() {
        let mut pool = Literals::new();
        pool.intern("hi");
        let second = pool.intern("next");
        assert_eq!(second % 4, 0);
        assert_eq!(pool.intern("hi"), u64::from(layout::LITERALS));
    }

    #[test]
    fn the_heap_starts_past_every_literal() {
        let mut pool = Literals::new();
        pool.intern("hello");
        assert!(pool.heap_base() >= u64::from(layout::LITERALS) + pool.data().len() as u64);
        assert_eq!(pool.heap_base() % u64::from(layout::ALIGN), 0);
    }

    #[test]
    fn an_empty_pool_still_puts_the_heap_past_the_fixed_regions() {
        let pool = Literals::new();
        assert_eq!(pool.heap_base(), u64::from(layout::LITERALS));
    }
}
