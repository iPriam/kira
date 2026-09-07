//! The `RawPtr` surface: the null constant a program spells, and comparing two
//! pointer words.
//!
//! A pointer word is the one value at the C seam a program both receives and
//! has to test. C answers `NULL` for "no window", "no adapter", "end of the
//! chain"; without a spelling for it, a Kira program has to write `RawPtr(0)`
//! and `rawPointerWord(p) == 0`, which are two casts standing in for one
//! meaning. `RawPtr.null` and `p == RawPtr.null` say it once.
//!
//! Comparison desugars to an integer comparison of the two words, the same
//! shape [`crate::typeck`] already gives enum equality and `distinct` equality:
//! the operator is Kira-side nominal typing, and the machine below it compares
//! words. No backend learns that pointers can be compared.

use kira_semantics_model::hir::{ConvertKind, HirExpr, HirExprId};
use kira_semantics_model::{IntSpelling, Type};
use kira_source::Span;
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::resolve_binary;

/// The member `RawPtr` answers, and the only one it has.
const NULL_MEMBER: &str = "null";

impl Analyzer<'_> {
    /// Recognizes `RawPtr.null`, the null pointer written rather than cast.
    ///
    /// Returns `None` when `base` does not name `RawPtr` at all, so the caller
    /// carries on with every other meaning a dotted name has. A local of the
    /// same name shadows the type, exactly as it does at `RawPtr(word)`.
    pub(crate) fn analyze_raw_pointer_member(
        &mut self,
        ctx: &FnCtx,
        base: ExprId,
        field: &str,
        span: Span,
    ) -> Option<HirExprId> {
        let Expr::Name { symbol, .. } = *self.tree.expr(base) else {
            return None;
        };
        let name = self.interner.resolve(symbol).to_owned();
        if Type::from_name(&name) != Some(Type::RawPtr) || ctx.resolve(&name).is_some() {
            return None;
        }
        if field != NULL_MEMBER {
            self.emit(
                span,
                "KSEM368",
                format!(
                    "`RawPtr` has no member `{field}`: it spells `RawPtr.null`, and \
                     `rawPointerWord(p)` reads the word itself"
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        Some(self.program.exprs.alloc(HirExpr::RawPtrNull))
    }

    /// Compares two pointer words, or `None` when the operands are not both
    /// pointers.
    ///
    /// Equality is the whole operator surface a pointer has here. Ordering is
    /// deliberately absent: C only defines `<` between two pointers into one
    /// object, Kira has no arithmetic that could produce such a pair, and an
    /// order over unrelated addresses answers a question about the allocator
    /// rather than about the program.
    ///
    /// Two `distinct` pointer types compare only to themselves, which is the
    /// rule every other `distinct` follows: an `Adapter` is the same adapter or
    /// it is not, and it is never the `Surface` beside it.
    pub(crate) fn analyze_pointer_equality(
        &mut self,
        op: BinaryOp,
        lhs: HirExprId,
        rhs: HirExprId,
        lt: Type,
        rt: Type,
    ) -> Option<HirExprId> {
        if !matches!(op, BinaryOp::Eq | BinaryOp::Ne) {
            return None;
        }
        if !self.is_pointer_word(lt) || !self.is_pointer_word(rt) {
            return None;
        }
        if (matches!(lt, Type::Distinct(_)) || matches!(rt, Type::Distinct(_))) && lt != rt {
            return None;
        }
        let word = Type::Int(IntSpelling::U64);
        let (hir_op, ty) = resolve_binary(op, word, word)?;
        let lhs = self.pointer_word(lhs);
        let rhs = self.pointer_word(rhs);
        Some(self.program.exprs.alloc(HirExpr::Binary {
            op: hir_op,
            lhs,
            rhs,
            ty,
        }))
    }

    /// Whether a type is one machine pointer word: `RawPtr`, an `@FFI.Pointer`,
    /// or a `distinct` over either.
    pub(crate) fn is_pointer_word(&self, ty: Type) -> bool {
        match ty {
            Type::RawPtr | Type::ForeignPtr(_) => true,
            Type::Distinct(_) => matches!(
                self.program.types.representation(ty),
                Type::RawPtr | Type::ForeignPtr(_)
            ),
            _ => false,
        }
    }

    /// The `U64` word of a pointer-typed expression, which is the value the
    /// comparison below actually reads.
    fn pointer_word(&mut self, value: HirExprId) -> HirExprId {
        self.program.exprs.alloc(HirExpr::Convert {
            operand: value,
            kind: ConvertKind::RawPtrToInt,
            ty: Type::Int(IntSpelling::U64),
        })
    }
}
