//! The `String` value surface: `String(x)`, and the three primitives a string
//! answers besides `.count`.
//!
//! `charAt`, `substring`, and `indexOf` all index **bytes**, the same units
//! `.count` measures. That is the one choice these four make together: a
//! program that carves text at a delimiter it found itself needs the index it
//! got back to mean the same thing to the operation it hands it to, and a
//! character count beside a byte index would not.
//!
//! Each traps rather than clamps on a range it cannot serve, so walking off the
//! end of a string fails the same way on every backend instead of producing a
//! value only one of them agrees with.

use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Recognizes and type-checks `String(x)`, the text rendering of a value.
    ///
    /// Returns `None` when the call is not this conversion — the callee is not
    /// `String`, or a local of that name shadows it — so the caller carries on
    /// to the ordinary call paths.
    pub(super) fn analyze_string_conversion(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        if name != "String" || ctx.resolve(name).is_some() {
            return None;
        }
        // From here the call form is the conversion, so this path owns it and
        // every branch returns `Some` — which is what keeps a mistake from also
        // being reported as an undefined function by the fallthrough.
        let values = Self::argument_values(args);
        if values.len() != 1 {
            for &value in &values {
                self.analyze_expr(ctx, value);
            }
            self.emit(
                span,
                "KSEM210",
                format!(
                    "a conversion to `String` takes exactly one argument, found {}",
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let operand = self.analyze_expr(ctx, values[0]);
        let operand_ty = self.program.expr(operand).type_of();
        if operand_ty == Type::Error {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        // Whatever `print` renders, this renders — that is the contract, and it
        // is why the set is exactly the printable scalars rather than a second
        // list that could drift from the first.
        let renderable = matches!(operand_ty, Type::Bool | Type::String) || operand_ty.is_numeric();
        if !renderable {
            self.emit(
                span,
                "KSEM209",
                format!(
                    "`{}` cannot be converted to `String`: only a scalar renders as text",
                    self.type_name(operand_ty)
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        Some(
            self.program
                .exprs
                .alloc(HirExpr::StringOf { value: operand }),
        )
    }

    /// Type-checks `s.<name>` where `s` is a `String` — the property side.
    ///
    /// `.count` is the only property. Naming one of the three methods without
    /// parentheses says so, which is more useful than "no such member".
    pub(crate) fn analyze_string_property(
        &mut self,
        text: HirExprId,
        name: &str,
        span: Span,
    ) -> HirExprId {
        if name == "count" {
            return self.program.exprs.alloc(HirExpr::StringLen { text });
        }
        if matches!(name, "charAt" | "substring" | "indexOf") {
            self.emit(
                span,
                "KSEM101",
                format!("`{name}` is a method: write `s.{name}(…)`"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.emit(
            span,
            "KSEM101",
            format!("a `String` has no member `{name}`"),
        );
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// Type-checks `s.<name>(args)` where `s` is a `String` — the method side.
    pub(crate) fn analyze_string_method(
        &mut self,
        ctx: &mut FnCtx,
        text: HirExprId,
        name: &str,
        span: Span,
        args: &[ExprId],
    ) -> HirExprId {
        if name == "count" {
            self.emit(
                span,
                "KSEM101",
                "`count` is a property: write `s.count`, without parentheses",
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let arity = match name {
            "charAt" | "indexOf" => 1,
            "substring" => 2,
            _ => {
                for &argument in args {
                    self.analyze_expr(ctx, argument);
                }
                self.emit(
                    span,
                    "KSEM101",
                    format!("a `String` has no method `{name}`"),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
        };
        if args.len() != arity {
            for &argument in args {
                self.analyze_expr(ctx, argument);
            }
            self.emit(
                span,
                "KSEM210",
                format!(
                    "`s.{name}` takes exactly {arity} argument(s), found {}",
                    args.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let expected = if name == "indexOf" {
            Type::String
        } else {
            Type::INT
        };
        let mut operands = Vec::with_capacity(arity);
        for &argument in args {
            let hir = self.analyze_expr(ctx, argument);
            let ty = self.program.expr(hir).type_of();
            if ty != Type::Error && ty != expected {
                let found = self.type_name(ty);
                let wanted = self.type_name(expected);
                self.emit(
                    self.tree.expr(argument).span(),
                    "KSEM211",
                    format!("`s.{name}` takes `{wanted}`, not `{found}`"),
                );
                return self.program.exprs.alloc(HirExpr::Error);
            }
            operands.push(hir);
        }
        match (name, operands.as_slice()) {
            ("charAt", [index]) => self.program.exprs.alloc(HirExpr::StringCharAt {
                text,
                index: *index,
            }),
            ("indexOf", [needle]) => self.program.exprs.alloc(HirExpr::StringIndexOf {
                text,
                needle: *needle,
            }),
            ("substring", [start, end]) => self.program.exprs.alloc(HirExpr::StringSubstring {
                text,
                start: *start,
                end: *end,
            }),
            _ => self.program.exprs.alloc(HirExpr::Error),
        }
    }
}
