//! `print(x)`: the one builtin that takes any printable value.
//!
//! Its own file because it answers a question no other call does — what can be
//! *rendered* — and that set is the contract `String(x)` mirrors.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{Builtin, Callee, HirBinaryOp, HirExpr, HirExprId};
use kira_syntax_model::ast::{ExprId, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    pub(super) fn analyze_print(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`print` takes exactly one argument, found {}", args.len()),
            );
        } else {
            let arg_ty = self.program.expr(args[0]).type_of();
            if arg_ty != Type::Error && !arg_ty.is_printable() {
                self.emit(
                    span,
                    "KSEM081",
                    format!(
                        "`print` cannot format a value of type `{}`",
                        self.type_name(arg_ty)
                    ),
                );
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Print),
            args: args.to_vec(),
            ty: Type::Void,
            writebacks: Vec::new(),
        })
    }

    /// `abort(message)`: the unrecoverable-failure primitive.
    ///
    /// Takes one `String` message and hard-traps (no unwind), so it renders as
    /// `Void` here even though control never returns. Its use is a deliberate
    /// end to a run — a failed assertion or a broken invariant — which the
    /// child-per-test runner records as a trap.
    pub(super) fn analyze_abort(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`abort` takes exactly one message, found {}", args.len()),
            );
        } else {
            let arg_ty = self.program.expr(args[0]).type_of();
            if arg_ty != Type::Error && arg_ty != Type::String {
                self.emit(
                    span,
                    "KSEM081",
                    format!(
                        "`abort` takes a `String` message, found `{}`",
                        self.type_name(arg_ty)
                    ),
                );
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Abort),
            args: args.to_vec(),
            ty: Type::Void,
            writebacks: Vec::new(),
        })
    }

    /// `code(enumValue)`: the enum's variant as its zero-based declaration
    /// index, an `Int`.
    ///
    /// The native form of what `@Derive(Tagged)` generated as `code_<Enum>` — a
    /// payloadless enum's stable code — earned by every enum from its shape with
    /// nothing to write. It reads the discriminant the language already carries
    /// ([`HirExpr::EnumTag`], the same tag `e == .V` compares), so it is one node
    /// on every backend rather than a synthesized function. A non-enum argument
    /// is refused here, naming the type.
    pub(super) fn analyze_code(&mut self, args: &[HirExprId], span: kira_source::Span) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`code` takes exactly one enum value, found {}", args.len()),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let arg_ty = self.program.expr(args[0]).type_of();
        if arg_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if !matches!(arg_ty, Type::Enum(_)) {
            self.emit(
                span,
                "KSEM081",
                format!(
                    "`code` takes an enum value, found `{}`",
                    self.type_name(arg_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.program.exprs.alloc(HirExpr::EnumTag { value: args[0] })
    }

    /// `compare(a, b) -> Ordering`: the three-way order of two `Ordered` values
    /// as Foundation's `Ordering` enum (`Less`, `Equal`, `Greater`).
    ///
    /// The native form of what `@Derive(Ordered)` generated as `compare_<Type>`,
    /// built from the pieces already native: the structural three-way compare
    /// answers a sign (`-1 / 0 / 1`), and `Ordering`'s three variants are exactly
    /// that sign plus one (`Less = 0`, `Equal = 1`, `Greater = 2`), so the result
    /// is `fromCode<Ordering>(compareSign + 1)` — no new primitive. A leaf that
    /// carries no total order is refused here, exactly as `<` refuses it.
    pub(super) fn analyze_compare(
        &mut self,
        args: &[HirExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        if args.len() != 2 {
            self.emit(
                span,
                "KSEM080",
                format!("`compare` takes exactly two values, found {}", args.len()),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let (left, right) = (args[0], args[1]);
        let left_ty = self.program.expr(left).type_of();
        let right_ty = self.program.expr(right).type_of();
        if left_ty == Type::Error || right_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if left_ty != right_ty {
            self.emit(
                span,
                "KSEM081",
                format!(
                    "`compare` takes two values of one type, found `{}` and `{}`",
                    self.type_name(left_ty),
                    self.type_name(right_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if let Some(reason) = self.ordered_refusal(left_ty) {
            self.emit(
                span,
                "KSEM391",
                format!(
                    "`{}` cannot be ordered with `compare`: {reason}",
                    self.type_name(left_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // `Ordering` is Foundation's, so it is in scope wherever `compare` is used
        // — a program that reaches ordering has imported Foundation. Its three
        // payload-less variants in declaration order are `Less`, `Equal`,
        // `Greater`, which is the sign-plus-one this builds.
        let Some(ordering) = self.program.types.enums().lookup_any("Ordering") else {
            self.emit(
                span,
                "KSEM081",
                "`compare` answers Foundation's `Ordering`, which is not in scope; import \
                 Foundation, or use `<` for a boolean order"
                    .to_owned(),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let count = self
            .program
            .types
            .enums()
            .get(ordering)
            .map_or(0, |def| def.variants.len());
        // The structural sign, shifted into `Ordering`'s variant range.
        let sign = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::CmpValue,
            lhs: left,
            rhs: right,
            ty: Type::INT,
        });
        let one = self.program.exprs.alloc(HirExpr::Int(1));
        let code = self.program.exprs.alloc(HirExpr::Binary {
            op: HirBinaryOp::AddInt,
            lhs: sign,
            rhs: one,
            ty: Type::INT,
        });
        let count = self.program.exprs.alloc(HirExpr::Int(count as i64));
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::FromCode),
            args: vec![code, count],
            ty: Type::Enum(ordering),
            writebacks: Vec::new(),
        })
    }

    /// `hash(value)`: fold a `Hashable` value into one `Int`.
    ///
    /// The native operation behind the `Hashable` trait — the fold twin of `==`
    /// — earned by every type whose leaves hash consistently with equality. A
    /// value that holds a leaf which cannot (a float, a pointer, a handle) is
    /// refused here, naming it, rather than folded into a hash that would
    /// disagree with `==`.
    pub(super) fn analyze_hash(&mut self, args: &[HirExprId], span: kira_source::Span) -> HirExprId {
        if args.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`hash` takes exactly one value, found {}", args.len()),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let arg_ty = self.program.expr(args[0]).type_of();
        if arg_ty == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        if let Some(reason) = self.hashable_refusal(arg_ty) {
            self.emit(
                span,
                "KSEM392",
                format!(
                    "`{}` cannot be hashed: {reason}",
                    self.type_name(arg_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::Hash),
            args: args.to_vec(),
            ty: Type::INT,
            writebacks: Vec::new(),
        })
    }

    /// `fromCode<E>(code)`: the payload-less variant of enum `E` whose
    /// declaration index is `code`, or the first variant for a code outside
    /// `0..variantCount`.
    ///
    /// The native form of what `@Derive(Tagged)` generated as `<E>_fromCode`.
    /// `E` comes from the one type argument and must be an enum whose every
    /// variant is payload-less — there is no payload to invent — and the code
    /// must be an `Int`. The enum's variant count is baked in as a second
    /// argument so the backend clamps and builds the variant with no type lookup
    /// ([`Builtin::FromCode`]).
    pub(super) fn analyze_from_code(
        &mut self,
        ctx: &mut FnCtx,
        type_args: &[TypeRefId],
        values: &[ExprId],
        span: kira_source::Span,
    ) -> HirExprId {
        if type_args.len() != 1 {
            self.emit(
                span,
                "KSEM081",
                format!(
                    "`fromCode` needs exactly one enum type argument, as `fromCode<Grade>(n)`, \
                     found {}",
                    type_args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let target = self.resolve_type_ref(type_args[0]);
        if target == Type::Error {
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let Type::Enum(enum_id) = target else {
            self.emit(
                span,
                "KSEM081",
                format!(
                    "`fromCode` builds an enum, but `{}` is not one",
                    self.type_name(target)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        // `fromCode` reconstructs a bare variant, so it needs an enum with no
        // payloads: there is nothing to build a payload from.
        let count = match self.program.types.enums().get(enum_id) {
            Some(def) => {
                if def.variants.iter().any(|variant| variant.payload.is_some()) {
                    self.emit(
                        span,
                        "KSEM081",
                        format!(
                            "`fromCode` needs an enum whose variants carry no payload, but `{}` \
                             has one — there is no payload to reconstruct",
                            self.type_name(target)
                        ),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                def.variants.len()
            }
            None => return self.program.exprs.alloc(HirExpr::Error),
        };
        if values.len() != 1 {
            self.emit(
                span,
                "KSEM080",
                format!("`fromCode` takes exactly one code, found {}", values.len()),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let code = self.analyze_expr(ctx, values[0]);
        let code_ty = self.program.expr(code).type_of();
        if code_ty != Type::Error && !matches!(code_ty, Type::Int(_)) {
            self.emit(
                span,
                "KSEM081",
                format!(
                    "`fromCode` takes an `Int` code, found `{}`",
                    self.type_name(code_ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let count = self.program.exprs.alloc(HirExpr::Int(count as i64));
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::Builtin(Builtin::FromCode),
            args: vec![code, count],
            ty: Type::Enum(enum_id),
            writebacks: Vec::new(),
        })
    }
}
