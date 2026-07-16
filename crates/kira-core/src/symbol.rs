//! String interning: `Symbol` handles plus the `Interner` that owns the text.
//!
//! Tree and model types store interned `Symbol` handles rather than borrowed
//! string slices, so no model type carries a lifetime.

use std::collections::HashMap;

/// A cheap, `Copy` handle to an interned string; resolve it through the [`Interner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Symbol(u32);

impl Symbol {
    /// The name an [`Interner`] hands back where a real name could not be read.
    ///
    /// Every interner reserves this at index 0 before anything else, which is
    /// what lets an error-resilient parser always have a name to carry: it needs
    /// one exactly when things have gone wrong, including when the interner is
    /// full and [`Interner::intern`] can hand out nothing new.
    pub const ERROR: Symbol = Symbol(0);

    /// Returns the raw index of this symbol inside its interner.
    pub fn as_u32(self) -> u32 {
        self.0
    }

    /// Rebuilds a symbol from a raw index previously obtained via [`Symbol::as_u32`].
    pub fn from_u32(raw: u32) -> Self {
        Symbol(raw)
    }
}

/// An interner cannot take another distinct string: every [`Symbol`] is taken.
///
/// A `Symbol` is a `u32`, so an interner holds at most `u32::MAX` strings.
/// Reaching that is a typed refusal rather than a panic — this type sits under
/// the whole compiler, and a library does not get to end its caller's process.
/// A caller that cannot proceed without a name uses [`Symbol::ERROR`], which is
/// always interned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this program has more distinct names than a Symbol can address")]
pub struct InternerFull;

/// Deduplicating string store handing out stable [`Symbol`] handles.
/// Derives `Clone`/`PartialEq`/`Eq` so a parsed program can carry its interner
/// as part of a salsa query result: tracked-function outputs must be `Clone`
/// and comparable for incremental memoization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interner {
    map: HashMap<String, Symbol>,
    strings: Vec<String>,
}

impl Default for Interner {
    /// Creates an interner holding only the reserved [`Symbol::ERROR`].
    fn default() -> Self {
        let mut interner = Interner {
            map: HashMap::new(),
            strings: Vec::new(),
        };
        // Reserved first, so it lands at index 0 and `Symbol::ERROR` names it.
        // Doing it here rather than on demand is what makes the fallback
        // available even once the interner is full.
        interner.strings.push(Interner::ERROR_TEXT.to_owned());
        interner
            .map
            .insert(Interner::ERROR_TEXT.to_owned(), Symbol::ERROR);
        interner
    }
}

impl Interner {
    /// The text [`Symbol::ERROR`] resolves to.
    pub const ERROR_TEXT: &'static str = "<error>";

    /// Creates an interner holding only the reserved [`Symbol::ERROR`].
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `text`, returning the existing handle when the string was seen
    /// before, or [`InternerFull`] when no handle is left to hand out.
    pub fn intern(&mut self, text: &str) -> Result<Symbol, InternerFull> {
        if let Some(&symbol) = self.map.get(text) {
            return Ok(symbol);
        }
        let raw = u32::try_from(self.strings.len()).map_err(|_| InternerFull)?;
        let symbol = Symbol(raw);
        self.strings.push(text.to_owned());
        self.map.insert(text.to_owned(), symbol);
        Ok(symbol)
    }

    /// Returns the string a symbol stands for.
    ///
    /// Panics when `symbol` was produced by a different interner.
    pub fn resolve(&self, symbol: Symbol) -> &str {
        &self.strings[symbol.0 as usize]
    }

    /// Number of distinct strings interned, including the reserved
    /// [`Interner::ERROR_TEXT`].
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when nothing but the reserved [`Interner::ERROR_TEXT`] is interned.
    pub fn is_empty(&self) -> bool {
        self.strings.len() <= 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interning_deduplicates_and_resolves() {
        let mut interner = Interner::new();
        let a = interner.intern("hello").expect("a fresh interner has room");
        let b = interner.intern("world").expect("a fresh interner has room");
        let a_again = interner.intern("hello").expect("a fresh interner has room");
        assert_eq!(a, a_again);
        assert_ne!(a, b);
        assert_eq!(interner.resolve(a), "hello");
        assert_eq!(interner.resolve(b), "world");
        // Two interned here, plus the reserved error name.
        assert_eq!(interner.len(), 3);
    }

    /// The fallback must exist before anything is interned: a caller reaches for
    /// it precisely when it could not read a name, and it must resolve even then.
    #[test]
    fn every_interner_reserves_the_error_symbol_first() {
        let interner = Interner::new();
        assert!(interner.is_empty());
        assert_eq!(interner.resolve(Symbol::ERROR), Interner::ERROR_TEXT);
        assert_eq!(Symbol::ERROR.as_u32(), 0);

        // Interning it explicitly finds the reserved one rather than a second.
        let mut interner = Interner::new();
        assert_eq!(
            interner.intern(Interner::ERROR_TEXT).expect("reserved"),
            Symbol::ERROR
        );
        assert!(interner.is_empty());
    }
}
