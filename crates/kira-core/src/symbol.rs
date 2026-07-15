//! String interning: `Symbol` handles plus the `Interner` that owns the text.
//!
//! Rust-port addition (no direct kira-zig counterpart): the Zig tree types
//! store `[]const u8` slices into source/arena memory; the Rust port stores
//! interned `Symbol` handles instead so no model type carries a lifetime.

use std::collections::HashMap;

/// A cheap, `Copy` handle to an interned string; resolve it through the [`Interner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl Symbol {
    /// Returns the raw index of this symbol inside its interner.
    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// Rebuilds a symbol from a raw index previously obtained via [`Symbol::as_u32`].
    pub fn from_u32(raw: u32) -> Self {
        Symbol(raw)
    }
}

/// Deduplicating string store handing out stable [`Symbol`] handles.
#[derive(Debug, Default)]
pub struct Interner {
    map: HashMap<String, Symbol>,
    strings: Vec<String>,
}

impl Interner {
    /// Creates an empty interner.
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `text`, returning the existing handle when the string was seen before.
    pub fn intern(&mut self, text: &str) -> Symbol {
        if let Some(&symbol) = self.map.get(text) {
            return symbol;
        }
        let symbol = Symbol(u32::try_from(self.strings.len()).expect("interner overflow"));
        self.strings.push(text.to_owned());
        self.map.insert(text.to_owned(), symbol);
        symbol
    }

    /// Returns the string a symbol stands for.
    ///
    /// Panics when `symbol` was produced by a different interner.
    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.0 as usize]
    }

    /// Number of distinct strings interned so far.
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when nothing has been interned yet.
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_deduplicates_and_resolves() {
        let mut interner = Interner::new();
        let a = interner.intern("hello");
        let b = interner.intern("world");
        let a_again = interner.intern("hello");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert_eq!(interner.resolve(a), "hello");
        assert_eq!(interner.resolve(b), "world");
        assert_eq!(interner.len(), 2);
    }
}
