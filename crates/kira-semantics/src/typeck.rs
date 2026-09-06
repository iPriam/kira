//! Expression type-checking and operator resolution.
//!
//! Each expression is lowered to a typed [`HirExpr`]. Operators resolve to
//! type-specific HIR variants (e.g. `+` on two `Int`s becomes `AddInt`), so no
//! backend re-derives operand types. Any operand that already analyzed to
//! `Error` short-circuits to another `Error`, suppressing cascades.
//!
//! Calls and construction live in [`calls`]: they share one question — what is
//! being called, and does the argument list fit its signature — and all but two
//! of them end up in the same argument checker.

use kira_semantics_model::{IntSpelling, Type};
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::resolve_binary;

mod calls;
mod compiler;
mod conditional;
mod env;
mod expr;
mod file_system;
mod labels;
mod memberwise;
mod native_state;
mod cast_results;
pub(crate) mod channels;
mod casts;
pub(crate) mod overloads;
mod print;
mod qualified;
mod struct_ops;

impl Analyzer<'_> {
    /// Type-checks an AST expression, returning its HIR handle.
    pub(crate) fn analyze_expr(&mut self, ctx: &mut FnCtx, id: ExprId) -> HirExprId {
        self.analyze_expr_expecting(ctx, id, None)
    }

    /// Type-checks an expression that sits where `expected` is wanted.
    ///
    /// The hint exists for exactly one construct: an **empty array literal**
    /// has no element to infer a type from, so `var xs: [Int] = []` can only
    /// work if the position's type reaches the literal. Every other expression
    /// ignores it and is typed bottom-up as before — this is a hint, not
    /// bidirectional type checking, and widening it into one would be a much
    /// larger change than the one construct that needs it.
    ///
    /// `None` means "nothing is expected here", which is different from
    /// expecting `Error`: the callers that have a type pass it, and the rest
    /// keep calling [`Analyzer::analyze_expr`].
    pub(crate) fn analyze_expr_expecting(
        &mut self,
        ctx: &mut FnCtx,
        id: ExprId,
        expected: Option<Type>,
    ) -> HirExprId {
        // A bare integer literal takes the floating-point type of the position
        // that asks for it. Named integer values remain distinct from Float;
        // only the literal spelling is context-sensitive.
        if matches!(expected, Some(Type::Float(_)))
            && let Expr::Int { value, .. } = self.tree.expr(id)
        {
            return self.program.exprs.alloc(HirExpr::Float(*value as f64));
        }
        // A bare function name is not an expression anywhere else — Kira has no
        // function type — so the one position that gives it a meaning is
        // recognized before the name is resolved as a value and reported
        // undefined.
        if let Some(callback) = self.callback_named_here(ctx, id, expected) {
            return callback;
        }
        let value = self.analyze_expr_inner(ctx, id, expected);
        self.coerce_construct_value(value, expected)
    }

    /// The callback value when `id` is a bare name, `expected` is an
    /// `@FFI.Callback` type, and the name is a top-level function rather than
    /// something in scope.
    ///
    /// A local wins: a variable holding a callback the program got from C is
    /// read as itself, exactly as it would be under any other expected type.
    fn callback_named_here(
        &mut self,
        ctx: &FnCtx,
        id: ExprId,
        expected: Option<Type>,
    ) -> Option<HirExprId> {
        let Expr::Name { symbol, span } = self.tree.expr(id) else {
            return None;
        };
        let (symbol, span) = (*symbol, *span);
        let name = self.interner.resolve(symbol).to_owned();
        if ctx.resolve(&name).is_some() {
            return None;
        }
        self.foreign_callback_value(&name, expected, span)
    }

    /// Type-checks a binary operation, threading expected types so a
    /// leading-dot operand resolves and desugaring enum equality to a tag
    /// comparison.
    ///
    /// A leading-dot member (`.Red`) has no bottom-up type: it resolves only
    /// against an expected one. So when exactly one operand is a leading dot,
    /// the *other* is analyzed first and its type becomes the dot's expectation
    /// — which is what makes `c == .Red` and `red != .Green` type-check without
    /// bidirectional inference in the general case.
    fn analyze_binary(
        &mut self,
        ctx: &mut FnCtx,
        op: BinaryOp,
        lhs: ExprId,
        rhs: ExprId,
        span: kira_source::Span,
    ) -> HirExprId {
        let lhs_is_dot = matches!(self.tree.expr(lhs), Expr::DotMember { .. });
        let rhs_is_dot = matches!(self.tree.expr(rhs), Expr::DotMember { .. });
        // Analyze the concrete side first when the other is a leading dot, so
        // the dot inherits its type.
        let (lhs_hir, rhs_hir) = if lhs_is_dot && !rhs_is_dot {
            let rhs_hir = self.analyze_expr(ctx, rhs);
            let rt = self.program.expr(rhs_hir).type_of();
            let lhs_hir = self.analyze_expr_expecting(ctx, lhs, Some(rt));
            (lhs_hir, rhs_hir)
        } else {
            let lhs_hir = self.analyze_expr(ctx, lhs);
            let lt = self.program.expr(lhs_hir).type_of();
            let rhs_hir = if rhs_is_dot {
                self.analyze_expr_expecting(ctx, rhs, Some(lt))
            } else {
                self.analyze_expr(ctx, rhs)
            };
            (lhs_hir, rhs_hir)
        };

        let lt = self.program.expr(lhs_hir).type_of();
        let rt = self.program.expr(rhs_hir).type_of();
        if lt == Type::Error || rt == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }

        // Enum equality is tag equality: `e == .V` becomes an `Int` comparison
        // of two discriminants, so no backend learns enums can be compared.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne) && matches!(lt, Type::Enum(_)) && lt == rt {
            return self.enum_equality(op == BinaryOp::Eq, lhs_hir, rhs_hir);
        }

        // Two pointer words compare as the words they are, which is what makes
        // `handle == RawPtr.null` a comparison rather than two casts.
        if let Some(compared) = self.analyze_pointer_equality(op, lhs_hir, rhs_hir, lt, rt) {
            return compared;
        }

        // Two values of one distinct type compare as the scalar word they are.
        // Equality is the whole operator surface a distinct type has: an id is
        // *the same id* or it is not, while adding two of them, ordering them,
        // or comparing one to its representation are the mistakes the type
        // exists to refuse. `resolve_binary` picks the machine comparison from
        // the representation, so no backend learns distinct types can be
        // compared.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
            && matches!(lt, Type::Distinct(_))
            && lt == rt
            && let Some((hir_op, ty)) = {
                let representation = self.program.types.representation(lt);
                resolve_binary(op, representation, representation)
            }
        {
            return self.program.exprs.alloc(HirExpr::Binary {
                op: hir_op,
                lhs: lhs_hir,
                rhs: rhs_hir,
                ty,
            });
        }

        if let Some(refused) = self.refuse_mixed_spellings(op, lhs_hir, rhs_hir, lt, rt, span) {
            return refused;
        }

        match resolve_binary(op, lt, rt) {
            Some((hir_op, ty)) => self.program.exprs.alloc(HirExpr::Binary {
                op: hir_op,
                lhs: lhs_hir,
                rhs: rhs_hir,
                ty,
            }),
            None => self
                .analyze_binary_operator_method(ctx, op, lhs, lhs_hir, rhs_hir, span)
                .unwrap_or_else(|| {
                    self.emit(
                        span,
                        "KSEM071",
                        format!(
                            "operator `{}` cannot combine `{}` and `{}`",
                            op.spelling(),
                            self.type_name(lt),
                            self.type_name(rt)
                        ),
                    );
                    self.program.exprs.alloc(HirExpr::Error)
                }),
        }
    }

    /// Refuses two integer operands of different spellings unless one is a
    /// bare literal the other's spelling can hold.
    ///
    /// A written width is the value's whole contract, so `Int` and `U8`
    /// operands do not mix by themselves: the program says which width the
    /// operation has, with a conversion such as `U8(x)`. A literal is the
    /// exception, because it has no width of its own until it is used, and
    /// it adapts to the other side when it fits. A shift count is the other
    /// exception: it is a count, not a value of the shifted kind.
    fn refuse_mixed_spellings(
        &mut self,
        op: BinaryOp,
        lhs: HirExprId,
        rhs: HirExprId,
        lt: Type,
        rt: Type,
        span: kira_source::Span,
    ) -> Option<HirExprId> {
        let (Type::Int(left), Type::Int(right)) = (lt, rt) else {
            return None;
        };
        if left == right || matches!(op, BinaryOp::Shl | BinaryOp::Shr) {
            return None;
        }
        let adapts = |literal: HirExprId, to: IntSpelling| match *self.program.expr(literal) {
            // A hexadecimal literal is a bit pattern, the one way a literal
            // can be negative: as a `U64` it names the unsigned value.
            HirExpr::Int(value) if to == IntSpelling::U64 && value < 0 => {
                to.holds(i128::from(value as u64))
            }
            HirExpr::Int(value) => to.holds(i128::from(value)),
            // `-101` is a negated literal, and adapts as the literal it is.
            HirExpr::Unary {
                op: kira_semantics_model::hir::HirUnaryOp::NegInt,
                operand,
                ..
            } => match *self.program.expr(operand) {
                HirExpr::Int(value) => to.holds(-i128::from(value)),
                _ => false,
            },
            _ => false,
        };
        if (left == IntSpelling::Plain && adapts(lhs, right))
            || (right == IntSpelling::Plain && adapts(rhs, left))
        {
            return None;
        }
        self.emit(
            span,
            "KSEM071",
            format!(
                "operator `{}` mixes `{}` and `{}`; convert one side to the other's spelling, \
                 such as `{}(…)`",
                op.spelling(),
                self.type_name(lt),
                self.type_name(rt),
                right.name()
            ),
        );
        Some(self.program.exprs.alloc(HirExpr::Error))
    }

    /// Resolves a bare name against the receiver's fields, for a method body
    /// that writes `step` rather than `self.step`.
    ///
    /// Returns `None` outside a method, or when the struct has no such field,
    /// so the caller still reports an undefined name.
    fn implicit_field(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        span: kira_source::Span,
    ) -> Option<HirExprId> {
        let owner = ctx.receiver?;
        let receiver = ctx.resolve("self")?;
        let base = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Struct(owner),
        });
        if self.construct_computed_member(owner, name) {
            return Some(self.analyze_construct_bridge_read(ctx, base, owner, name, span));
        }
        let def = self.program.types.structs().get(owner)?;
        let index = def.field_index(name)?;
        let ty = def.field(index)?.ty;
        let read = self.program.exprs.alloc(HirExpr::Field { base, index, ty });
        self.note_drop_extraction(read, span);
        Some(read)
    }

    /// Analyzes a default initializer in a declaration-owned scope.
    ///
    /// The scope is isolated from the construction site, but the local arena is
    /// the caller's arena. That distinction matters for defaults which construct
    /// another value: their synthesized field-binding statements and local
    /// reads must belong to the function that will execute them, not to a
    /// throwaway probe context.
    pub(crate) fn analyze_default_in(
        &mut self,
        ctx: &mut FnCtx,
        default: ExprId,
        declared: Option<Type>,
    ) -> HirExprId {
        ctx.push_isolated_scope();
        let value = self.analyze_expr_expecting(ctx, default, declared);
        ctx.pop_scope();
        value
    }

    /// Analyzes a declaration default for the eager validation pass. Callers
    /// that need an executable value use [`Self::analyze_default_in`] so any
    /// locals introduced by a nested construct are owned by the caller.
    pub(crate) fn analyze_default(&mut self, default: ExprId, declared: Option<Type>) -> HirExprId {
        let mut empty = FnCtx::new(Type::Void);
        self.analyze_default_in(&mut empty, default, declared)
    }
}
