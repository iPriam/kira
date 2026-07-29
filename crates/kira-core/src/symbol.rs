//! String interning: `Symbol` handles, the `Interner` that mints them, and the
//! resolve-only [`Names`] table a finished program reads them through.
//!
//! Tree and model types store interned `Symbol` handles rather than borrowed
//! string slices, so no model type carries a lifetime.
//!
//! # Why an interner has a base
//!
//! A program is parsed one file at a time so an unchanged file's parse can be
//! reused across compilations. Each file therefore mints its own symbols, and
//! they have to mean the same thing once the files are assembled — so a file's
//! interner starts numbering at the base its position in the program gives it,
//! and the program's [`Names`] table is the files' strings concatenated in that
//! same order. Nothing is renumbered when a program is assembled, which is what
//! keeps a reused parse reusable.
//!
//! The consequence is that two files that both write `Point` hold two distinct
//! symbols for it. **Never compare symbols from different files** — resolve
//! both and compare the text.

use std::collections::HashMap;
use std::sync::Arc;

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

/// What each [`Symbol`] of a finished program stands for: a resolve-only table.
///
/// Separate from [`Interner`] on purpose. A program's table is the *assembly*
/// of its files' interners — their strings concatenated in base order — so its
/// deduplicating map is gone and interning another string into it could only
/// mint a handle that collides with a later file's. Making that impossible by
/// type is why this exists: nothing hands out a symbol here.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Names {
    strings: Vec<Arc<str>>,
}

impl Names {
    /// An empty table.
    ///
    /// Empty rather than pre-seeded with the reserved name: the program's first
    /// file is based at zero and reserves it as its own first string, so
    /// seeding it here would push every one of that file's symbols off by one.
    /// [`Names::resolve`] answers with the reserved name for a handle this
    /// table does not cover, so a program with no files at all still resolves
    /// [`Symbol::ERROR`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends every string one file interned, keeping the handles it minted
    /// valid.
    ///
    /// Total by construction rather than by check: a file whose interner was
    /// created with [`Interner::with_base`] at this table's current length
    /// lands each of its strings at exactly the index its symbol names. The
    /// strings are shared rather than copied, so assembling a program from
    /// files parsed earlier costs a reference count per name.
    pub fn append(&mut self, other: &Names) {
        self.strings.extend(other.strings.iter().cloned());
    }

    /// The base the next file's interner must be created with.
    #[must_use]
    pub fn next_base(&self) -> u32 {
        // A table cannot hold more strings than a `Symbol` can address, because
        // every string in it came from an interner that refused to mint one.
        u32::try_from(self.strings.len()).unwrap_or(u32::MAX)
    }

    /// Returns the string a symbol stands for, or the reserved error name when
    /// the symbol belongs to no file of this program.
    #[must_use]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        self.strings
            .get(symbol.0 as usize)
            .map_or(Interner::ERROR_TEXT, |text| &**text)
    }

    /// Number of strings the program's files interned between them, including
    /// each file's reserved [`Interner::ERROR_TEXT`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when no file has contributed a string yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.is_empty()
    }
}

/// Deduplicating string store handing out stable [`Symbol`] handles.
/// Derives `Clone`/`PartialEq`/`Eq` so a parsed program can carry its interner
/// as part of a salsa query result: tracked-function outputs must be `Clone`
/// and comparable for incremental memoization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Interner {
    map: HashMap<Arc<str>, Symbol>,
    strings: Vec<Arc<str>>,
    /// The handle this interner's first string was minted at.
    ///
    /// Zero for a standalone interner; the program-wide count of everything
    /// interned before it for a file being parsed into a larger program.
    base: u32,
}

impl Default for Interner {
    /// Creates an interner holding only the reserved [`Symbol::ERROR`].
    fn default() -> Self {
        Self::with_base(0)
    }
}

impl Interner {
    /// The text [`Symbol::ERROR`] resolves to.
    pub const ERROR_TEXT: &'static str = "<error>";

    /// Creates an interner holding only the reserved [`Symbol::ERROR`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates an interner whose first handle is `base`.
    ///
    /// What lets one file of a program be parsed on its own: its symbols are
    /// already the program's, so assembling the files renumbers nothing.
    #[must_use]
    pub fn with_base(base: u32) -> Self {
        let mut interner = Interner {
            map: HashMap::new(),
            strings: Vec::new(),
            base,
        };
        // Reserved first, so it lands at this interner's base. Doing it here
        // rather than on demand is what makes the fallback available even once
        // the interner is full.
        let error: Arc<str> = Arc::from(Interner::ERROR_TEXT);
        interner.strings.push(Arc::clone(&error));
        interner.map.insert(error, Symbol(base));
        interner
    }

    /// Interns `text`, returning the existing handle when the string was seen
    /// before, or [`InternerFull`] when no handle is left to hand out.
    pub fn intern(&mut self, text: &str) -> Result<Symbol, InternerFull> {
        if let Some(&symbol) = self.map.get(text) {
            return Ok(symbol);
        }
        let offset = u32::try_from(self.strings.len()).map_err(|_| InternerFull)?;
        let raw = self.base.checked_add(offset).ok_or(InternerFull)?;
        let symbol = Symbol(raw);
        let owned: Arc<str> = Arc::from(text);
        self.strings.push(Arc::clone(&owned));
        self.map.insert(owned, symbol);
        Ok(symbol)
    }

    /// Returns the string a symbol stands for, or the reserved error name when
    /// the symbol belongs to another file.
    #[must_use]
    pub fn resolve(&self, symbol: Symbol) -> &str {
        symbol
            .0
            .checked_sub(self.base)
            .and_then(|offset| self.strings.get(offset as usize))
            .map_or(Interner::ERROR_TEXT, |text| &**text)
    }

    /// The handle the next distinct string would be minted at.
    #[must_use]
    pub fn next_base(&self) -> u32 {
        self.base
            .saturating_add(u32::try_from(self.strings.len()).unwrap_or(u32::MAX))
    }

    /// Number of distinct strings interned, including the reserved
    /// [`Interner::ERROR_TEXT`].
    #[must_use]
    pub fn len(&self) -> usize {
        self.strings.len()
    }

    /// True when nothing but the reserved [`Interner::ERROR_TEXT`] is interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.strings.len() <= 1
    }

    /// This interner's strings as a resolve-only table.
    ///
    /// Only meaningful for an interner based at zero — a single-file parse.
    #[must_use]
    pub fn into_names(self) -> Names {
        let mut names = Names {
            strings: Vec::with_capacity(self.strings.len()),
        };
        names.strings.extend(self.strings);
        names
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

    /// The whole point of a base: a file parsed on its own already holds the
    /// program's handles, so assembling the files renumbers nothing.
    #[test]
    fn based_interners_assemble_without_renumbering() {
        let mut first = Interner::with_base(0);
        let alpha = first.intern("alpha").expect("room");
        let beta = first.intern("beta").expect("room");

        let mut second = Interner::with_base(first.next_base());
        let gamma = second.intern("gamma").expect("room");
        // The same spelling in a second file is a second symbol, by design.
        let beta_again = second.intern("beta").expect("room");
        assert_ne!(beta, beta_again);

        let mut names = Names::new();
        names.append(&first.clone().into_names());
        names.append(&second.clone().into_names());

        assert_eq!(names.resolve(Symbol::ERROR), Interner::ERROR_TEXT);
        assert_eq!(names.resolve(alpha), "alpha");
        assert_eq!(names.resolve(beta), "beta");
        assert_eq!(names.resolve(gamma), "gamma");
        assert_eq!(names.resolve(beta_again), "beta");
        assert_eq!(names.next_base(), second.next_base());
    }

    /// A based interner resolves its own handles and answers the reserved name
    /// for one belonging to another file, rather than underflowing.
    #[test]
    fn a_based_interner_resolves_only_its_own_handles() {
        let mut second = Interner::with_base(7);
        let name = second.intern("name").expect("room");
        assert_eq!(second.resolve(name), "name");
        assert_eq!(second.resolve(Symbol::ERROR), Interner::ERROR_TEXT);
        assert_eq!(second.resolve(Symbol::from_u32(6)), Interner::ERROR_TEXT);
        assert_eq!(second.resolve(Symbol::from_u32(99)), Interner::ERROR_TEXT);
    }

    /// An empty program still resolves the one handle an error-resilient
    /// parser can always reach for.
    #[test]
    fn an_empty_name_table_still_resolves_the_error_symbol() {
        let names = Names::new();
        assert!(names.is_empty());
        assert_eq!(names.resolve(Symbol::ERROR), Interner::ERROR_TEXT);
        assert_eq!(names.next_base(), 0);
    }
}
