//! The whole-file container and the arena handles every node references.

use la_arena::{Arena, Idx};

use crate::ast::{Expr, Item, Stmt, TypeRef};

/// Handle to an expression stored in a [`KslTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`KslTree`].
pub type StmtId = Idx<Stmt>;
/// Handle to a written type stored in a [`KslTree`].
pub type TypeRefId = Idx<TypeRef>;

/// One parsed KSL file: its top-level items and the arenas they index into.
///
/// One tree is one file. Unlike Kira's own syntax tree it never spans several,
/// because a KSL `import` names a module the resolver loads and parses into its
/// own tree — the trees stay separate and semantics joins them.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct KslTree {
    /// Top-level items, in source order.
    pub items: Vec<Item>,
    /// Arena backing every [`ExprId`].
    pub exprs: Arena<Expr>,
    /// Arena backing every [`StmtId`].
    pub stmts: Arena<Stmt>,
    /// Arena backing every [`TypeRefId`].
    pub types: Arena<TypeRef>,
}

impl KslTree {
    /// Creates an empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The expression `id` handles.
    #[must_use]
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[id]
    }

    /// The statement `id` handles.
    #[must_use]
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[id]
    }

    /// The written type `id` handles.
    #[must_use]
    pub fn type_ref(&self, id: TypeRefId) -> &TypeRef {
        &self.types[id]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_source::Span;

    #[test]
    fn an_arena_handle_reads_back_the_node_it_was_given() {
        let mut tree = KslTree::new();
        let id = tree.exprs.alloc(Expr::Int {
            value: 7,
            span: Span::new(0, 1),
        });
        assert_eq!(
            tree.expr(id),
            &Expr::Int {
                value: 7,
                span: Span::new(0, 1)
            }
        );
    }
}
