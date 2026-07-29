//! The whole-program container, the per-file node chunk it is assembled from,
//! and the arena handle types every node references.

use super::{Expr, Item, Stmt, TypeRef};
use kira_source::SourceId;
use la_arena::{Arena, Idx, RawIdx};
use std::sync::Arc;

/// Handle to an expression stored in a [`SyntaxTree`].
pub type ExprId = Idx<Expr>;
/// Handle to a statement stored in a [`SyntaxTree`].
pub type StmtId = Idx<Stmt>;
/// Handle to a written type reference stored in a [`SyntaxTree`].
pub type TypeRefId = Idx<TypeRef>;

/// How many handles one page of a [`SyntaxTree`]'s file index covers.
///
/// A handle is turned into the file holding it by reading that file straight
/// out of a page table rather than by searching: node lookup is the analyzer's
/// innermost operation, and a binary search over every file of a large program
/// would be paid millions of times per compilation.
const PAGE: u32 = 256;

/// The first handle each of a file's three arenas is numbered from.
///
/// Handles are **program-global** even though the arenas behind them are per
/// file: a file is parsed against the base its position in the program gives
/// it, so assembling a program renumbers nothing and an unchanged file's parse
/// stays valid in the next compilation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct NodeBase {
    /// The first [`ExprId`] this file mints.
    pub exprs: u32,
    /// The first [`StmtId`] this file mints.
    pub stmts: u32,
    /// The first [`TypeRefId`] this file mints.
    pub types: u32,
}

/// One file's syntax nodes, numbered from the base its position gave it.
///
/// Built by the parser one file at a time and shared by handle afterwards, so
/// a compilation that re-reads an unchanged file does no work for it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FileNodes {
    base: NodeBase,
    exprs: Arena<Expr>,
    stmts: Arena<Stmt>,
    types: Arena<TypeRef>,
}

/// The raw number behind a handle.
fn raw_of<T>(id: Idx<T>) -> u32 {
    u32::from(id.into_raw())
}

/// The handle `raw` names, as an index into a file arena based at `base`.
fn local_of<T>(raw: u32, base: u32) -> Idx<T> {
    Idx::from_raw(RawIdx::from(raw.wrapping_sub(base)))
}

/// The program-global handle `local` names in a file arena based at `base`.
fn global_of<T>(local: Idx<T>, base: u32) -> Idx<T> {
    Idx::from_raw(RawIdx::from(base.wrapping_add(raw_of(local))))
}

impl FileNodes {
    /// Starts a file's nodes at `base`.
    #[must_use]
    pub fn new(base: NodeBase) -> Self {
        Self {
            base,
            exprs: Arena::new(),
            stmts: Arena::new(),
            types: Arena::new(),
        }
    }

    /// The base the next file must start from.
    #[must_use]
    pub fn end(&self) -> NodeBase {
        NodeBase {
            exprs: self.base.exprs.saturating_add(self.exprs.len() as u32),
            stmts: self.base.stmts.saturating_add(self.stmts.len() as u32),
            types: self.base.types.saturating_add(self.types.len() as u32),
        }
    }

    /// Interns an expression node, returning its program-global handle.
    pub fn add_expr(&mut self, expr: Expr) -> ExprId {
        global_of(self.exprs.alloc(expr), self.base.exprs)
    }

    /// Interns a statement node, returning its program-global handle.
    pub fn add_stmt(&mut self, stmt: Stmt) -> StmtId {
        global_of(self.stmts.alloc(stmt), self.base.stmts)
    }

    /// Interns a type reference node, returning its program-global handle.
    pub fn add_type(&mut self, ty: TypeRef) -> TypeRefId {
        global_of(self.types.alloc(ty), self.base.types)
    }

    /// Borrows an expression this file holds.
    #[must_use]
    pub fn expr(&self, id: ExprId) -> &Expr {
        &self.exprs[local_of(raw_of(id), self.base.exprs)]
    }

    /// Borrows a statement this file holds.
    #[must_use]
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        &self.stmts[local_of(raw_of(id), self.base.stmts)]
    }

    /// Borrows a statement this file holds, for replacement in place.
    ///
    /// The parser desugars a construct member's tail into a `return` by
    /// rewriting the statement the block's last handle names, which is the one
    /// place a node is written after it is built.
    pub fn stmt_mut(&mut self, id: StmtId) -> &mut Stmt {
        &mut self.stmts[local_of(raw_of(id), self.base.stmts)]
    }

    /// Borrows a type reference this file holds.
    #[must_use]
    pub fn type_ref(&self, id: TypeRefId) -> &TypeRef {
        &self.types[local_of(raw_of(id), self.base.types)]
    }
}

/// One file as a program is assembled from it: its items and its nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct FilePart {
    /// The file every span in these items and nodes is attributed to.
    pub source: SourceId,
    /// The file's top-level items, in source order.
    pub items: Arc<[Item]>,
    /// The file's nodes, already numbered into the program.
    pub nodes: Arc<FileNodes>,
}

/// Maps a program-global handle to the file that holds it.
#[derive(Debug, Clone, PartialEq, Default)]
struct FileIndex {
    /// `bases[i]` is file *i*'s first handle; the last entry is the total.
    bases: Vec<u32>,
    /// The first file any handle on page *p* can belong to.
    pages: Vec<u32>,
}

impl FileIndex {
    /// Builds the index from each file's first handle and the total count.
    fn build(bases: Vec<u32>) -> Self {
        let total = bases.last().copied().unwrap_or(0);
        let page_count = (total / PAGE + 1) as usize;
        let mut pages = Vec::with_capacity(page_count);
        let mut file = 0usize;
        for page in 0..page_count {
            let first = page as u32 * PAGE;
            while file + 1 < bases.len() && bases[file + 1] <= first {
                file += 1;
            }
            pages.push(file as u32);
        }
        Self { bases, pages }
    }

    /// The file holding `raw`, or the last file when `raw` names no node —
    /// which leaves the out-of-range handle to fail on the arena itself,
    /// exactly as it did when one arena spanned the program.
    fn file_of(&self, raw: u32) -> usize {
        let page = (raw / PAGE) as usize;
        let mut file = self.pages.get(page).copied().unwrap_or(0) as usize;
        // At most one file boundary per page is crossed for a file bigger than
        // a page, and the loop is what covers the several tiny files that can
        // share one.
        while file + 1 < self.bases.len() && self.bases[file + 1] <= raw {
            file += 1;
        }
        file
    }
}

/// A whole parsed program: the top-level items of every file it is built from,
/// plus each file's node arenas.
///
/// One tree spans **many files**, and every handle is global across them: the
/// arenas stay per file so an unchanged file's parse can be reused in the next
/// compilation, while the numbering is the program's so nothing has to be
/// renumbered when the files are assembled.
///
/// Imports are file-scoped, so resolution has to know which file each item came
/// from; `items` and `item_sources` carry that, and they are kept in step by
/// construction — assembly pushes to both — so `item_sources[i]` always
/// describes `items[i]`. Both are private for exactly that reason; read them
/// through [`SyntaxTree::items`] and [`SyntaxTree::items_with_source`].
#[derive(Debug, Clone, Default)]
pub struct SyntaxTree {
    /// Top-level items, dependency modules first and each file's items in
    /// source order.
    items: Vec<Item>,
    /// The file each item came from, positionally aligned with `items`.
    item_sources: Vec<SourceId>,
    /// Each file's nodes, in the order the files were assembled.
    files: Vec<Arc<FileNodes>>,
    expr_index: FileIndex,
    stmt_index: FileIndex,
    type_index: FileIndex,
}

impl PartialEq for SyntaxTree {
    /// Compares files by identity first: two programs that reused the same
    /// file's parse share the very same nodes, and walking them again is the
    /// cost this whole arrangement exists to avoid.
    fn eq(&self, other: &Self) -> bool {
        self.items == other.items
            && self.item_sources == other.item_sources
            && self.files.len() == other.files.len()
            && self
                .files
                .iter()
                .zip(&other.files)
                .all(|(left, right)| Arc::ptr_eq(left, right) || left == right)
    }
}

impl SyntaxTree {
    /// Creates an empty tree.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Assembles a program from its files, in the order they were parsed.
    ///
    /// Callers pass dependencies before dependents, because a struct field may
    /// only name a struct declared earlier.
    #[must_use]
    pub fn assemble(parts: Vec<FilePart>) -> Self {
        let mut items = Vec::new();
        let mut item_sources = Vec::new();
        let mut files = Vec::with_capacity(parts.len());
        let mut expr_bases = Vec::with_capacity(parts.len() + 1);
        let mut stmt_bases = Vec::with_capacity(parts.len() + 1);
        let mut type_bases = Vec::with_capacity(parts.len() + 1);
        let mut end = NodeBase::default();
        for part in parts {
            items.extend(part.items.iter().cloned());
            item_sources.extend(std::iter::repeat_n(part.source, part.items.len()));
            expr_bases.push(part.nodes.base.exprs);
            stmt_bases.push(part.nodes.base.stmts);
            type_bases.push(part.nodes.base.types);
            end = part.nodes.end();
            files.push(part.nodes);
        }
        expr_bases.push(end.exprs);
        stmt_bases.push(end.stmts);
        type_bases.push(end.types);
        Self {
            items,
            item_sources,
            files,
            expr_index: FileIndex::build(expr_bases),
            stmt_index: FileIndex::build(stmt_bases),
            type_index: FileIndex::build(type_bases),
        }
    }

    /// Every top-level item, in order.
    #[must_use]
    pub fn items(&self) -> &[Item] {
        &self.items
    }

    /// Every top-level item paired with the file it was written in.
    ///
    /// Total by construction: the pairing comes from zipping two vectors that
    /// only assembly ever grows, so there is no index to get wrong and nothing
    /// to unwrap.
    pub fn items_with_source(&self) -> impl Iterator<Item = (SourceId, &Item)> {
        self.item_sources.iter().copied().zip(self.items.iter())
    }

    /// How many leading files these two programs literally share.
    ///
    /// Identity, not equality: a file counted here was parsed once and
    /// assembled into both programs, which is the whole point of numbering a
    /// file's handles from its position rather than renumbering them at
    /// assembly. What a caller reads is how much of a program's syntax the
    /// previous compilation already had.
    #[must_use]
    pub fn shared_prefix(&self, other: &Self) -> usize {
        self.files
            .iter()
            .zip(&other.files)
            .take_while(|(left, right)| Arc::ptr_eq(left, right))
            .count()
    }

    /// Every expression the program holds, in file order then arena order.
    ///
    /// For a caller asking a question about the whole program — is any node of
    /// this shape here at all — rather than walking down from an item.
    pub fn exprs(&self) -> impl Iterator<Item = (ExprId, &Expr)> {
        self.files.iter().flat_map(|file| {
            file.exprs
                .iter()
                .map(|(local, expr)| (global_of(local, file.base.exprs), expr))
        })
    }

    /// Borrows an expression by handle.
    #[must_use]
    pub fn expr(&self, id: ExprId) -> &Expr {
        self.files[self.expr_index.file_of(raw_of(id))].expr(id)
    }

    /// Borrows a statement by handle.
    #[must_use]
    pub fn stmt(&self, id: StmtId) -> &Stmt {
        self.files[self.stmt_index.file_of(raw_of(id))].stmt(id)
    }

    /// Borrows a type reference by handle.
    #[must_use]
    pub fn type_ref(&self, id: TypeRefId) -> &TypeRef {
        self.files[self.type_index.file_of(raw_of(id))].type_ref(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kira_source::Span;

    /// Builds a file of `count` distinguishable integer literals.
    fn file(source: SourceId, base: NodeBase, count: u32) -> (FilePart, Vec<ExprId>) {
        let mut nodes = FileNodes::new(base);
        let ids: Vec<ExprId> = (0..count)
            .map(|value| {
                nodes.add_expr(Expr::Int {
                    value: i64::from(source.value() * 1_000_000 + value),
                    span: Span::new(0, 0),
                })
            })
            .collect();
        let part = FilePart {
            source,
            items: Arc::from(Vec::new()),
            nodes: Arc::new(nodes),
        };
        (part, ids)
    }

    /// The point of the whole arrangement: a handle minted while parsing one
    /// file finds that file's node once the program is assembled, whatever
    /// else is in the program.
    #[test]
    fn a_handle_finds_the_file_that_minted_it() {
        // Deliberately spanning several pages, and a tiny file between two
        // large ones so a page holds more than one file boundary.
        let (first, first_ids) = file(SourceId::new(1), NodeBase::default(), 700);
        let (second, second_ids) = file(SourceId::new(2), first.nodes.end(), 3);
        let (third, third_ids) = file(SourceId::new(3), second.nodes.end(), 500);
        let tree = SyntaxTree::assemble(vec![first, second, third]);

        for (source, ids) in [(1u32, first_ids), (2, second_ids), (3, third_ids)] {
            for (value, id) in ids.into_iter().enumerate() {
                let expected = i64::from(source * 1_000_000 + value as u32);
                assert!(
                    matches!(tree.expr(id), Expr::Int { value, .. } if *value == expected),
                    "file {source} node {value} came back as another file's",
                );
            }
        }
    }

    /// Assembly renumbers nothing, so the same file's part can be reused in a
    /// second program and its handles still mean what they meant.
    #[test]
    fn a_reused_file_part_keeps_its_handles() {
        let (shared, shared_ids) = file(SourceId::new(1), NodeBase::default(), 400);
        let (tail, _) = file(SourceId::new(2), shared.nodes.end(), 10);
        let (other_tail, _) = file(SourceId::new(3), shared.nodes.end(), 900);

        let one = SyntaxTree::assemble(vec![shared.clone(), tail]);
        let two = SyntaxTree::assemble(vec![shared, other_tail]);
        for id in shared_ids {
            assert_eq!(one.expr(id), two.expr(id));
        }
    }

    /// Items keep the file they were written in, which is what the file-scoped
    /// import gate reads.
    #[test]
    fn assembly_keeps_each_item_with_its_file() {
        let named = |source: SourceId| FilePart {
            source,
            items: Arc::from(vec![Item::Unsupported(crate::ast::UnsupportedItem {
                keyword: "?",
                span: Span::new(0, 0),
            })]),
            nodes: Arc::new(FileNodes::new(NodeBase::default())),
        };
        let tree = SyntaxTree::assemble(vec![named(SourceId::new(4)), named(SourceId::new(9))]);
        let sources: Vec<SourceId> = tree.items_with_source().map(|(source, _)| source).collect();
        assert_eq!(sources, vec![SourceId::new(4), SourceId::new(9)]);
    }
}
