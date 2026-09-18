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

use kira_runtime_abi::NumberOp;
use kira_semantics_model::hir::{FieldOrder, HirBinaryOp, HirExpr, HirExprId, HirUnaryOp};
use kira_semantics_model::{FloatSpelling, IntSpelling, Type};
use kira_syntax_model::ast::{BinaryOp, Expr, ExprId, UnaryOp};

use crate::analyze::{Analyzer, FnCtx};
use crate::operators::{resolve_binary, resolve_unary};

mod calls;
mod cast_results;
mod casts;
pub(crate) mod channels;
mod compiler;
mod conditional;
mod env;
mod expr_inner;
mod file_system;
mod labels;
mod match_expr;
mod memberwise;
mod native_state;
pub(crate) mod overloads;
mod print;
mod qualified;
mod struct_ops;

/// Whether a type is a bare integer or float literal — the one non-distinct
/// operand a distinct type pairs with, because it has no width of its own.
fn is_plain_numeric(ty: Type) -> bool {
    matches!(
        ty,
        Type::Int(IntSpelling::Plain) | Type::Float(FloatSpelling::Plain)
    )
}

impl Analyzer<'_> {
    /// Type-checks an AST expression, returning its HIR handle.
    pub(crate) fn analyze_expr(&mut self, ctx: &mut FnCtx, id: ExprId) -> HirExprId {
        self.analyze_expr_expecting(ctx, id, None)
    }

    /// Type-checks a tuple value and lowers it to an ordinary synthetic struct.
    /// Numeric field names make `.0`, `.1`, and so on use normal field access.
    pub(crate) fn analyze_tuple_value(
        &mut self,
        ctx: &mut FnCtx,
        elements: &[ExprId],
    ) -> HirExprId {
        let values: Vec<HirExprId> = elements
            .iter()
            .map(|&element| self.analyze_expr(ctx, element))
            .collect();
        if !(2..=4).contains(&values.len()) {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let types: Vec<Type> = values
            .iter()
            .map(|&value| self.program.expr(value).type_of())
            .collect();
        let Type::Struct(struct_id) = self.tuple_type(&types) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id,
            fields: values,
            order: FieldOrder::Declared,
        })
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
    pub(crate) fn analyze_binary(
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

        // Two `Number`s take the decimal operators, which are their own
        // instructions rather than the integer or float ones — there is no
        // implicit `Int`/`Float` mixing, so a `Number` beside anything else
        // falls through to the mixed-operand diagnostic below.
        if lt == Type::Number && rt == Type::Number {
            return self.analyze_number_binary(op, lhs_hir, rhs_hir, span);
        }

        // Structural equality on an aggregate: a struct, an array, or a
        // payload-carrying enum compared to another value of its own type. Enum
        // equality against a payload-less variant literal stays a tag
        // comparison — the common `c == .Red` never allocates — and everything
        // else walks the value; a leaf with no equality is refused here, naming
        // it, rather than compiled into a wrong identity comparison.
        if matches!(op, BinaryOp::Eq | BinaryOp::Ne)
            && lt == rt
            && matches!(lt, Type::Struct(_) | Type::Array(_) | Type::Enum(_))
        {
            return self.analyze_structural_equality(op, lt, lhs_hir, rhs_hir, span);
        }

        // Structural ordering on an aggregate: a struct, an array, or an enum
        // compared to another value of its own type with `<`, `<=`, `>`, `>=`.
        // Every aggregate whose leaves are all totally ordered walks the value
        // lexicographically; a leaf with no order is refused here, naming it,
        // rather than compiled into a wrong or non-deterministic comparison.
        if matches!(op, BinaryOp::Lt | BinaryOp::Le | BinaryOp::Gt | BinaryOp::Ge)
            && lt == rt
            && matches!(lt, Type::Struct(_) | Type::Array(_) | Type::Enum(_))
        {
            return self.analyze_structural_ordering(op, lt, lhs_hir, rhs_hir, span);
        }

        // Two pointer words compare as the words they are, which is what makes
        // `handle == RawPtr.null` a comparison rather than two casts.
        if let Some(compared) = self.analyze_pointer_equality(op, lhs_hir, rhs_hir, lt, rt) {
            return compared;
        }

        // A distinct type carries the whole operator surface of its
        // representation. Arithmetic, bitwise, and shift operators yield the
        // distinct type again — the value stays inside the type it was minted in
        // — while comparisons and equality yield `Bool`. A bare literal adapts
        // into a distinct type the way it adapts into a written integer width,
        // so `stream - 1` and `stream <= 0` read as themselves. Two *different*
        // distinct types, and a distinct type against a written representation
        // value, share no operator: those are the mistakes the type exists to
        // refuse, so they fall through to the mixed-operand diagnostic.
        // `resolve_binary` picks the machine op from the representation, so no
        // backend learns a distinct type takes operators at all.
        if (matches!(lt, Type::Distinct(_)) || matches!(rt, Type::Distinct(_)))
            && let Some((hir_op, ty)) = self.resolve_distinct_binary(op, lt, rt)
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

    /// Builds `==` / `!=` on two values of one aggregate type.
    ///
    /// An enum compared against a payload-less variant literal, or an enum with
    /// no payload-carrying variant at all, stays a tag comparison — correct and
    /// allocation-free. Every other aggregate walks its value with
    /// [`HirBinaryOp::EqValue`], once its type is known to conform to
    /// `Equatable`; a non-comparable leaf is refused here, naming it, rather
    /// than lowered to a wrong identity compare.
    fn analyze_structural_equality(
        &mut self,
        op: BinaryOp,
        ty: Type,
        lhs_hir: HirExprId,
        rhs_hir: HirExprId,
        span: kira_source::Span,
    ) -> HirExprId {
        let is_eq = op == BinaryOp::Eq;
        if let Type::Enum(id) = ty {
            let payloadless_enum = self
                .program
                .types
                .enums()
                .get(id)
                .is_none_or(|def| def.variants.iter().all(|variant| variant.payload.is_none()));
            if payloadless_enum
                || self.is_payloadless_variant_literal(lhs_hir)
                || self.is_payloadless_variant_literal(rhs_hir)
            {
                return self.enum_equality(is_eq, lhs_hir, rhs_hir);
            }
        }
        if let Some(reason) = self.equatable_refusal(ty) {
            self.emit(
                span,
                "KSEM390",
                format!(
                    "`{}` cannot be compared with `{}`: {reason}",
                    self.type_name(ty),
                    op.spelling()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let hir_op = if is_eq {
            HirBinaryOp::EqValue
        } else {
            HirBinaryOp::NeValue
        };
        self.program.exprs.alloc(HirExpr::Binary {
            op: hir_op,
            lhs: lhs_hir,
            rhs: rhs_hir,
            ty: Type::Bool,
        })
    }

    /// Builds `<` / `<=` / `>` / `>=` on two values of one aggregate type.
    ///
    /// The aggregate walks its value with [`HirBinaryOp::CmpValue`], a
    /// three-way structural compare answering an `Int` sign, once its type is
    /// known to conform to `Ordered`; the written operator becomes the ordinary
    /// integer comparison of that sign against zero. A leaf with no total order
    /// is refused here, naming it, rather than lowered to an order read off an
    /// address.
    fn analyze_structural_ordering(
        &mut self,
        op: BinaryOp,
        ty: Type,
        lhs_hir: HirExprId,
        rhs_hir: HirExprId,
        span: kira_source::Span,
    ) -> HirExprId {
        if let Some(reason) = self.ordered_refusal(ty) {
            self.emit(
                span,
                "KSEM391",
                format!(
                    "`{}` cannot be ordered with `{}`: {reason}",
                    self.type_name(ty),
                    op.spelling()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let compare = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::CmpValue,
            lhs: lhs_hir,
            rhs: rhs_hir,
            ty: Type::INT,
        });
        let zero = self.program.exprs.alloc(HirExpr::Int(0));
        let int_op = match op {
            BinaryOp::Lt => HirBinaryOp::LtInt,
            BinaryOp::Le => HirBinaryOp::LeInt,
            BinaryOp::Gt => HirBinaryOp::GtInt,
            BinaryOp::Ge => HirBinaryOp::GeInt,
            _ => unreachable!("analyze_structural_ordering is only reached for the four orderings"),
        };
        self.program.exprs.alloc(HirExpr::Binary {
            op: int_op,
            lhs: compare,
            rhs: zero,
            ty: Type::Bool,
        })
    }

    /// Whether `id` is a payload-less variant literal such as `.Red`, the one
    /// operand shape enum equality folds to a bare tag comparison.
    fn is_payloadless_variant_literal(&self, id: HirExprId) -> bool {
        matches!(self.program.expr(id), HirExpr::EnumNew { payload: None, .. })
    }

    /// Resolves a binary operator where at least one operand is a distinct
    /// type, mapping each distinct operand to its representation and rewrapping
    /// the result.
    ///
    /// Returns `None` — declining the distinct path so the ordinary
    /// diagnostics report it — for two *different* distinct types, and for a
    /// distinct type paired with a written representation value rather than a
    /// bare literal. A bare `Int`/`Float` (spelling [`IntSpelling::Plain`] /
    /// [`FloatSpelling::Plain`]) is the one non-distinct operand that pairs,
    /// exactly as it adapts to a written integer width.
    /// One binary operator on two `Number`s, lowered to a `NumberOperation`.
    ///
    /// Arithmetic answers a `Number`, the orderings and equality answer `Bool`,
    /// and `!=` is `!(a == b)`. `%` and the bitwise and shift operators have no
    /// decimal meaning, so they are refused here rather than silently dropped.
    fn analyze_number_binary(
        &mut self,
        op: BinaryOp,
        lhs: HirExprId,
        rhs: HirExprId,
        span: kira_source::Span,
    ) -> HirExprId {
        use BinaryOp as B;
        let (number_op, ty) = match op {
            B::Add => (NumberOp::Add, Type::Number),
            B::Sub => (NumberOp::Subtract, Type::Number),
            B::Mul => (NumberOp::Multiply, Type::Number),
            B::Div => (NumberOp::Divide, Type::Number),
            B::Lt => (NumberOp::Less, Type::Bool),
            B::Le => (NumberOp::LessOrEqual, Type::Bool),
            B::Gt => (NumberOp::Greater, Type::Bool),
            B::Ge => (NumberOp::GreaterOrEqual, Type::Bool),
            B::Eq => (NumberOp::Equal, Type::Bool),
            B::Ne => {
                let equal = self.program.exprs.alloc(HirExpr::NumberOperation {
                    op: NumberOp::Equal,
                    operands: vec![lhs, rhs],
                    ty: Type::Bool,
                });
                return self.program.exprs.alloc(HirExpr::Unary {
                    op: HirUnaryOp::Not,
                    operand: equal,
                    ty: Type::Bool,
                });
            }
            _ => {
                self.emit(span, "KSEM071", "a `Number` has no such operator");
                return self.program.exprs.alloc(HirExpr::Error);
            }
        };
        self.program.exprs.alloc(HirExpr::NumberOperation {
            op: number_op,
            operands: vec![lhs, rhs],
            ty,
        })
    }

    fn resolve_distinct_binary(
        &self,
        op: BinaryOp,
        lt: Type,
        rt: Type,
    ) -> Option<(HirBinaryOp, Type)> {
        let lt_distinct = matches!(lt, Type::Distinct(_));
        let rt_distinct = matches!(rt, Type::Distinct(_));
        // Two different distinct types share nothing.
        if lt_distinct && rt_distinct && lt != rt {
            return None;
        }
        // A non-distinct operand must be a bare literal, never a written width
        // or another concrete type: a distinct type does not mix with its
        // representation by itself.
        if !lt_distinct && !is_plain_numeric(lt) {
            return None;
        }
        if !rt_distinct && !is_plain_numeric(rt) {
            return None;
        }
        let lrep = if lt_distinct {
            self.program.types.representation(lt)
        } else {
            lt
        };
        let rrep = if rt_distinct {
            self.program.types.representation(rt)
        } else {
            rt
        };
        let (hir_op, res) = resolve_binary(op, lrep, rrep)?;
        // A comparison or equality answers `Bool`; a value-producing operator
        // keeps the distinct type. A shift takes its result from the left
        // operand alone, matching how its width is the left's.
        let ty = if res == Type::Bool {
            Type::Bool
        } else if matches!(op, BinaryOp::Shl | BinaryOp::Shr) {
            if lt_distinct { lt } else { lrep }
        } else if lt_distinct {
            lt
        } else {
            rt
        };
        Some((hir_op, ty))
    }

    /// Resolves a unary operator, unwrapping a distinct operand to its
    /// representation and keeping the distinct type on the result.
    pub(crate) fn resolve_unary_typed(
        &self,
        op: UnaryOp,
        operand: Type,
    ) -> Option<(HirUnaryOp, Type)> {
        if matches!(operand, Type::Distinct(_)) {
            let representation = self.program.types.representation(operand);
            let (hir_op, _) = resolve_unary(op, representation)?;
            return Some((hir_op, operand));
        }
        resolve_unary(op, operand)
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
                op: HirUnaryOp::NegInt,
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
