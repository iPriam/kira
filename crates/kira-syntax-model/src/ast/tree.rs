//! The whole-file container and the arena handle types every node references.

use super::{Expr, Item, Stmt, TypeRef};
use la_arena::{Arena, Idx};

/// Handle to an expression stored in a [`SyntaxTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`SyntaxTree`].
pub type StmtId = Idx<Stmt>;
/// Handle to a written type reference stored in a [`SyntaxTree`].
pub type TypeRefId = Idx<TypeRef>;

/// A whole parsed source file: its top-level items plus the node arenas.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SyntaxTree {
    /// Top-level items in source order.
    pub items: Vec<Item>,
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
