//! The whole-file container and the arena handle types every node references.

use super::{Expr, Item, Stmt, TypeRef};
use kira_source::SourceId;
use la_arena::{Arena, Idx};

/// Handle to an expression stored in a [`SyntaxTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`SyntaxTree`].
pub type StmtId = Idx<Stmt>;
/// Handle to a written type reference stored in a [`SyntaxTree`].
pub type TypeRefId = Idx<TypeRef>;

/// A whole parsed program: the top-level items of every file it is built from,
/// plus the node arenas they all share.
///
/// One tree spans **many files**. Imports are file-scoped, so resolution has to
/// know which file each item came from; `items` and `item_sources` carry that,
/// and they are kept in step by construction — [`SyntaxTree::push_item`] is the
/// only way to add an item and it pushes to both, so `item_sources[i]` always
/// describes `items[i]`. Both are private for exactly that reason; read them
/// through [`SyntaxTree::items`] and [`SyntaxTree::items_with_source`].
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyntaxTree {
    /// Top-level items, dependency modules first and each file's items in
    /// source order.
    items: Vec<Item>,
    /// The file each item came from, positionally aligned with `items`.
    item_sources: Vec<SourceId>,
    /// Arena backing every [`ExprId`].
    pub exprs: Arena<Expr>,
    /// Arena backing every [`StmtId`].
    pub stmts: Arena<Stmt>,
    /// Arena backing every [`TypeRefId`].
    pub types: Arena<TypeRef>,
}

impl SyntaxTree {
    /// Creates an empty tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends a top-level item, recording the file it was written in.
    pub fn push_item(&mut self, source: SourceId, item: Item) {
        self.items.push(item);
        self.item_sources.push(source);
    }

    /// Every top-level item, in order.
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Every top-level item paired with the file it was written in.
    ///
    /// Total by construction: the pairing comes from zipping two vectors that
    /// only [`SyntaxTree::push_item`] ever grows, so there is no index to get
    /// wrong and nothing to unwrap.
    pub fn items_with_source(&self) -> impl Iterator<Item = (SourceId, &Item)> {
        self.item_sources.iter().copied().zip(self.items.iter())
    }

    /// Interns an expression node, returning its handle.
    pub fn add_expr(&mut self, expr: Expr) -> ExprId {
        self.exprs.alloc(expr)
    }

    /// Interns a statement node, returning its handle.
    pub fn add_stmt(&mut self, stmt: Stmt) -> StmtId {
        self.stmts.alloc(stmt)
    }

    /// Borrows an expression by handle.
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id]
    }

    /// Borrows a statement by handle.
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id]
    }

    /// Interns a type reference node, returning its handle.
    pub fn add_type(&mut self, ty: TypeRef) -> TypeRefId {
        self.types.alloc(ty)
    }

    /// Borrows a type reference by handle.
    pub fn type_ref(&self, id: TypeRefId) -> &TypeRef {
        &self.types[id]
    }
}
