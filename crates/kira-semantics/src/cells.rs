//! Boxing a captured `var`, and reading and writing it afterwards.
//!
//! A closure that captures a mutable binding has to share storage with the
//! frame that declared it. Nothing else in this language does: a struct copies
//! deeply, an array's block is shared only until a writer buys its own, and an
//! enum is never written through at all. So a captured `var` moves into a
//! **capture cell** — a share-counted box — and every read and write of that
//! binding goes through the box from its declaration onward.
//!
//! # The whole rewrite
//!
//! ```text
//!   var total = 0          ->  let total = CellNew(0)          (Type::Cell(Int))
//!   … total …              ->  CellGet(total)
//!   total = e              ->  CellSet(total, e)
//!   total[i] = e           ->  let t = CellGet(total)
//!                              t[i] = e
//!                              CellSet(total, t)
//!   { … total … }          ->  the closure captures the *cell*, by handle
//! ```
//!
//! The decision is made at the declaration, from [`FnCtx::must_box`], because
//! that is the only point early enough: the closure that captures the binding
//! is analyzed later, and by then every read of it has already been lowered.
//!
//! # Why a write through the cell is read-modify-write
//!
//! The last case above is the one worth stating. A cell hands out **owned**
//! reads, so `t` is this frame's copy of what the box held — and for an array
//! it is a second handle on one block. Writing `t[i]` runs the ordinary
//! uniqueness check, which sees the block shared and buys elements of its own;
//! storing `t` back is what makes that new block the one the cell holds.
//!
//! Skipping the store-back would leave the write in a copy nobody can see:
//! silently wrong, with no crash to find it by. Doing the write in place
//! instead — handing generated code a pointer into the payload slot — would
//! mean sharing the *array* with unrelated holders, which is exactly the
//! sharing value semantics forbids. Sharing the cell is intended; sharing what
//! is inside it is not.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId, HirPlace, HirStmt, LocalId};

use crate::analyze::{Analyzer, FnCtx};

/// Whether a capture cell can hold a value of `ty`.
///
/// Everything with a runtime value except the three the box has nowhere to put
/// and the one it must not: `Void` names no value, `CString` is borrowed C
/// storage that is seam-only, a `NativeState` handle belongs to a host that
/// counts its own holds, and a cell of a cell is a shape the analyzer never
/// mints. A binding this refuses is simply not boxed, and a closure capturing
/// one is refused where every uncapturable binding is (`KSEM117`).
pub(crate) fn cell_can_hold(ty: Type) -> bool {
    !matches!(
        ty,
        Type::Void | Type::Error | Type::CString | Type::NativeState(_) | Type::Cell(_)
    )
}

impl Analyzer<'_> {
    /// The type a cell-backed local holds, or `None` when it is not one.
    pub(crate) fn cell_inner(&self, ctx: &FnCtx, local: LocalId) -> Option<Type> {
        self.program.types.cell_inner(ctx.local_type(local))
    }

    /// Reads `local`, going through the box when it is cell-backed.
    ///
    /// The one place a *user-written* name becomes a value, so a boxed binding
    /// reads as what it holds and nothing downstream learns the box exists.
    pub(crate) fn read_local(&mut self, ctx: &FnCtx, local: LocalId) -> HirExprId {
        match self.cell_inner(ctx, local) {
            Some(ty) => self.program.exprs.alloc(HirExpr::CellGet { local, ty }),
            None => {
                let ty = ctx.local_type(local);
                self.program.exprs.alloc(HirExpr::Local { local, ty })
            }
        }
    }

    /// Wraps a binding's initializer in a fresh cell, returning the cell type.
    ///
    /// One box per execution of the declaration, which is what makes a `var`
    /// declared inside a loop a fresh binding each turn — and what lets a
    /// closure made on one turn keep that turn's storage after the loop has
    /// moved on.
    pub(crate) fn box_binding(&mut self, init: HirExprId, held: Type) -> (HirExprId, Type) {
        let cell_ty = self.program.types.cell_of(held);
        let expr = self.program.exprs.alloc(HirExpr::CellNew {
            value: init,
            ty: cell_ty,
        });
        (expr, cell_ty)
    }

    /// Rewrites a place rooted at a cell-backed local into one a backend can
    /// write through.
    ///
    /// Two shapes, and the difference is whether the *binding* is being
    /// replaced or written into:
    ///
    /// - Replacing it (`total = e`) keeps the cell as the root and leaves the
    ///   caller to emit a [`HirStmt::CellSet`]. The place's type becomes what
    ///   the cell holds, so the value is checked against that rather than
    ///   against a type no source spells.
    /// - Writing into it (`xs[i] = e`, `xs.append(e)`, a mutating method, a
    ///   `borrow mut` argument) hoists `let t = CellGet(cell)` ahead of the
    ///   statement, roots the place at `t`, and defers `CellSet(cell, t)` until
    ///   after it.
    ///
    /// A statement mentioning one cell twice gets **one** temporary, not two.
    /// That is not an optimization: two temporaries would make two writes
    /// through the same storage look like writes to different storage, and the
    /// aliasing check that refuses those would stop seeing them.
    pub(crate) fn cell_place(
        &mut self,
        ctx: &mut FnCtx,
        local: LocalId,
        through_path: bool,
        replaces_binding: bool,
    ) -> Option<(HirPlace, Type)> {
        let held = self.cell_inner(ctx, local)?;
        if !through_path && replaces_binding {
            return Some((
                HirPlace {
                    local,
                    path: Vec::new(),
                },
                held,
            ));
        }
        let temp = match ctx.cell_temp(local) {
            Some(existing) => existing,
            None => {
                let temp = ctx.declare_hidden(held, true);
                let read = self
                    .program
                    .exprs
                    .alloc(HirExpr::CellGet { local, ty: held });
                let prologue = self.program.stmts.alloc(HirStmt::Let {
                    local: temp,
                    init: read,
                });
                ctx.hoist_stmt(prologue);
                // The value read out is moved back in whole. `HirExpr::Local`
                // of the temporary copies it — a share for an array, which the
                // uniqueness check has already made private — and the
                // temporary's own hold goes when its slot is reused or the
                // frame ends, leaving the cell the sole owner again.
                let write_back = self.program.exprs.alloc(HirExpr::Local {
                    local: temp,
                    ty: held,
                });
                let epilogue = self.program.stmts.alloc(HirStmt::CellSet {
                    local,
                    value: write_back,
                });
                ctx.defer_stmt(epilogue);
                ctx.note_cell_temp(local, temp);
                temp
            }
        };
        Some((
            HirPlace {
                local: temp,
                path: Vec::new(),
            },
            held,
        ))
    }
}
