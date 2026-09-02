//! `distinct Name = Representation` types and the program's table of them.
//!
//! A distinct type is **nominal to the type checker and absent below it**. It
//! has its own row here — so `TabId` and `BookmarkId` over one `U32` are three
//! types, not one — while the row records the representation every layer under
//! semantics sees instead. `kira-ir` reads that representation and erases the
//! rows, so no bytecode, no LLVM type, and no C signature ever learns a
//! distinct type existed.
//!
//! # Why the representation is a scalar
//!
//! The predicates that decide copying, moving, and dropping —
//! [`Type::is_trivially_copyable`], [`Type::moves_on_bind`],
//! [`Type::is_scalar`] — are methods on [`Type`] alone, with no table to ask.
//! A distinct type answers all of them the same way its representation does,
//! and it can only do that without a lookup while every representation answers
//! them alike. Restricting the representation to the scalar words — the
//! integers, the floats, `Bool`, and `RawPtr` — is what makes those answers
//! total, and it is the whole of the feature's purpose: a distinct type names
//! an identity that happens to be a number.

use super::Type;

/// Index of a distinct type within a [`DistinctTable`].
///
/// Only a [`DistinctTable`] mints one, so an id always names a row of the table
/// it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DistinctId(u32);

impl DistinctId {
    /// This id as an index, for a consumer keying its own per-type data.
    pub fn index(self) -> u32 {
        self.0
    }
}

/// One `distinct Name = Representation` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct DistinctDef {
    /// The type's name, as written.
    pub name: String,
    /// The type this one *is* at run time.
    ///
    /// Always a scalar word, and never another distinct type: a chain is
    /// flattened when the row is minted, so reading this once gives the
    /// representation rather than the next link.
    pub representation: Type,
}

/// Every distinct type a program declares, indexed by [`DistinctId`].
///
/// Deliberately **not** interning: two declarations of the same name over the
/// same representation are two types, which is the entire point. A row is
/// minted per declaration and compared by id.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct DistinctTable {
    defs: Vec<DistinctDef>,
    // Kept in step with `defs` by `declare_owned`, the only way to add one:
    // `"{package}::{name}"`, or the bare name for the program's own row.
    index: std::collections::HashMap<String, DistinctId>,
    // Index-aligned with `defs`: the declaring package, or `None` for one of
    // the program's own files.
    owners: Vec<Option<String>>,
    // Index-aligned with `defs`: the declaring module inside its package.
    modules: Vec<String>,
}

fn owned_key(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(owner) => format!("{owner}::{name}"),
        None => name.to_owned(),
    }
}

impl DistinctTable {
    /// Creates an empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares a row owned by the program's own files.
    pub fn declare(&mut self, name: String, representation: Type) -> Option<DistinctId> {
        self.declare_owned(None, name, representation)
    }

    /// Declares a row filed under `owner`; a repeat within one owner is
    /// refused with `None`, while two owners may each declare the name.
    pub fn declare_owned(
        &mut self,
        owner: Option<&str>,
        name: String,
        representation: Type,
    ) -> Option<DistinctId> {
        let key = owned_key(owner, &name);
        if self.index.contains_key(&key) {
            return None;
        }
        let id = DistinctId(u32::try_from(self.defs.len()).ok()?);
        self.index.insert(key, id);
        self.defs.push(DistinctDef {
            name,
            representation,
        });
        self.owners.push(owner.map(str::to_owned));
        self.modules.push(String::new());
        Some(id)
    }

    /// The row `owner` declared under `name`.
    pub fn lookup_owned(&self, owner: Option<&str>, name: &str) -> Option<DistinctId> {
        self.index.get(&owned_key(owner, name)).copied()
    }

    /// The package that declared `id`, or `None` for the program's own row.
    pub fn owner_of(&self, id: DistinctId) -> Option<&str> {
        self.owners.get(id.0 as usize).and_then(Option::as_deref)
    }

    /// Records the module `id` was declared in.
    pub fn set_module(&mut self, id: DistinctId, module: &str) {
        if let Some(slot) = self.modules.get_mut(id.0 as usize) {
            *slot = module.to_owned();
        }
    }

    /// The module `id` was declared in.
    pub fn module_of(&self, id: DistinctId) -> &str {
        self.modules
            .get(id.0 as usize)
            .map_or("", String::as_str)
    }

    /// The row behind an id.
    pub fn get(&self, id: DistinctId) -> Option<&DistinctDef> {
        self.defs.get(id.0 as usize)
    }

    /// The representation behind an id.
    pub fn representation(&self, id: DistinctId) -> Option<Type> {
        self.get(id).map(|def| def.representation)
    }

    /// Every row, as `(id, def)`, in declaration order.
    pub fn rows(&self) -> impl Iterator<Item = (DistinctId, &DistinctDef)> {
        self.defs
            .iter()
            .enumerate()
            .map(|(index, def)| (DistinctId(index as u32), def))
    }

    /// How many distinct types the program declares.
    pub fn len(&self) -> usize {
        self.defs.len()
    }

    /// Whether the program declares none.
    pub fn is_empty(&self) -> bool {
        self.defs.is_empty()
    }
}

/// Whether `ty` may be the representation of a distinct type.
///
/// The scalar words and nothing else — see the module docs for why the set is
/// closed here rather than left to the declaration site. Another distinct type
/// is accepted by the *declaration*, which flattens the chain before it reaches
/// a row, so it is not in this set.
pub fn is_representable(ty: Type) -> bool {
    matches!(
        ty,
        Type::Int(_) | Type::Float(_) | Type::Bool | Type::RawPtr
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_declarations_of_one_shape_are_two_types() {
        let mut table = DistinctTable::new();
        let tab = table.declare("TabId".to_owned(), Type::INT).expect("row");
        let bookmark = table
            .declare("BookmarkId".to_owned(), Type::INT)
            .expect("row");
        assert_ne!(tab, bookmark, "a distinct type is nominal, not structural");
        assert_ne!(Type::Distinct(tab), Type::Distinct(bookmark));
        assert_eq!(table.representation(tab), Some(Type::INT));
    }

    #[test]
    fn the_scalar_words_are_the_representable_set() {
        assert!(is_representable(Type::INT));
        assert!(is_representable(Type::FLOAT));
        assert!(is_representable(Type::Bool));
        assert!(is_representable(Type::RawPtr));
        assert!(!is_representable(Type::String));
        assert!(!is_representable(Type::Void));
        assert!(!is_representable(Type::Any));
    }
}
