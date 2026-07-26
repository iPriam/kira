//! Statements: scoping, assignability, and whether a body returns.

use kira_ksl_syntax_model::ast::{Block, Stmt};
use kira_shader_model::{ScalarType, Type};

use super::Checker;
use crate::diagnostics;
use crate::model::{CheckedExprKind, CheckedStmt, CheckedStmtId};

impl Checker<'_> {
    /// Checks a block in its own scope.
    pub(crate) fn block(&mut self, block: &Block) -> Vec<CheckedStmtId> {
        self.scopes.push(std::collections::HashMap::new());
        let checked = self.stmts(&block.stmts);
        self.scopes.pop();
        checked
    }

    /// Checks each statement in order, so a later one sees earlier bindings.
    fn stmts(&mut self, ids: &[kira_ksl_syntax_model::tree::StmtId]) -> Vec<CheckedStmtId> {
        let mut checked = Vec::with_capacity(ids.len());
        for &id in ids {
            if let Some(stmt) = self.stmt(id) {
                checked.push(self.module.stmts.alloc(stmt));
            }
        }
        checked
    }

    /// Checks one statement.
    fn stmt(&mut self, id: kira_ksl_syntax_model::tree::StmtId) -> Option<CheckedStmt> {
        match self.tree_stmt(id) {
            Stmt::Let {
                name,
                ty,
                init,
                span,
            } => {
                let name = self.name(name);
                let annotated = ty.map(|id| self.resolve(id));
                let checked_init = init.map(|id| {
                    let expr = self.expr(id);
                    match &annotated {
                        Some(expected) => self.coerce(expr, expected, span),
                        None => expr,
                    }
                });
                let ty = match (annotated, checked_init) {
                    (Some(ty), _) => ty,
                    (None, Some(id)) => self.module.expr(id).ty.clone(),
                    (None, None) => {
                        self.reporter.error(
                            span,
                            diagnostics::TYPE_MISMATCH,
                            format!(
                                "`{name}` has neither a type nor a value, so its type is unknown"
                            ),
                        );
                        Type::Void
                    }
                };
                self.bind(&name, ty.clone());
                Some(CheckedStmt::Let {
                    name,
                    ty,
                    init: checked_init,
                })
            }
            Stmt::Assign {
                target,
                value,
                span,
            } => {
                let target = self.expr(target);
                let expected = self.module.expr(target).ty.clone();
                if !self.is_assignable(target) {
                    self.reporter.error(
                        span,
                        diagnostics::NOT_ASSIGNABLE,
                        "this cannot be written to: only a local, a stage output, or a \
                         `read_write` storage element can be assigned",
                    );
                }
                let value = self.expr(value);
                let value = self.coerce(value, &expected, span);
                Some(CheckedStmt::Assign { target, value })
            }
            Stmt::If {
                cond,
                then,
                otherwise,
                span,
            } => {
                let cond = self.condition(cond, span);
                let then = self.block(&then);
                let otherwise = otherwise.map(|id| match self.tree_stmt(id) {
                    // An `else if` is one statement; a plain `else` is a block.
                    Stmt::Block(block) => self.block(&block),
                    _ => self
                        .stmt(id)
                        .map(|stmt| vec![self.module.stmts.alloc(stmt)])
                        .unwrap_or_default(),
                });
                Some(CheckedStmt::If {
                    cond,
                    then,
                    otherwise,
                })
            }
            Stmt::While { cond, body, span } => {
                let cond = self.condition(cond, span);
                let body = self.block(&body);
                Some(CheckedStmt::While { cond, body })
            }
            Stmt::Return { value, span } => {
                let expected = self.result.clone();
                match (value, &expected) {
                    (Some(id), _) => {
                        let value = self.expr(id);
                        let value = self.coerce(value, &expected, span);
                        Some(CheckedStmt::Return(Some(value)))
                    }
                    (None, Type::Void) => Some(CheckedStmt::Return(None)),
                    (None, expected) => {
                        self.reporter.error(
                            span,
                            diagnostics::TYPE_MISMATCH,
                            format!(
                                "this function returns `{}`, so `return` needs a value",
                                describe(expected)
                            ),
                        );
                        Some(CheckedStmt::Return(None))
                    }
                }
            }
            Stmt::Block(block) => {
                // A bare block introduces a scope and nothing else; splicing it
                // in would leak its bindings, so it stays one statement.
                let inner = self.block(&block);
                Some(CheckedStmt::If {
                    cond: self.always_true(),
                    then: inner,
                    otherwise: None,
                })
            }
            Stmt::Expr { expr, .. } => {
                let expr = self.expr(expr);
                Some(CheckedStmt::Expr(expr))
            }
        }
    }

    /// Checks a condition, which must be a `Bool`.
    fn condition(
        &mut self,
        id: kira_ksl_syntax_model::tree::ExprId,
        span: kira_source::Span,
    ) -> crate::model::CheckedExprId {
        let checked = self.expr(id);
        let ty = self.module.expr(checked).ty.clone();
        if ty != Type::Scalar(ScalarType::Bool) && ty != Type::Void {
            self.reporter.error(
                span,
                diagnostics::TYPE_MISMATCH,
                format!(
                    "a condition must be a `Bool`, but this is `{}`",
                    describe(&ty)
                ),
            );
        }
        checked
    }

    /// A `true` the checker made up, for a bare block's scope.
    fn always_true(&mut self) -> crate::model::CheckedExprId {
        self.module.exprs.alloc(crate::model::CheckedExpr {
            ty: Type::Scalar(ScalarType::Bool),
            kind: CheckedExprKind::Const(crate::model::ConstValue::Bool(true)),
        })
    }

    /// The syntax statement `id` handles.
    fn tree_stmt(&self, id: kira_ksl_syntax_model::tree::StmtId) -> Stmt {
        self.tree().stmt(id).clone()
    }

    /// Binds `name` in the innermost scope.
    pub(crate) fn bind(&mut self, name: &str, ty: Type) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), ty);
        }
    }

    /// The type `name` is bound to, searching innermost outward.
    pub(crate) fn lookup(&self, name: &str) -> Option<Type> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).cloned())
    }

    /// Whether `id` names a place a body may write through.
    ///
    /// The root of the chain decides it: a local is always writable, a
    /// `read_write` storage element is, and everything else — a uniform, a
    /// texture, a call's result — is not.
    fn is_assignable(&self, id: crate::model::CheckedExprId) -> bool {
        match &self.module.expr(id).kind {
            CheckedExprKind::Local(_) => true,
            CheckedExprKind::Resource(name) => self
                .resources
                .get(name)
                .is_some_and(|binding| binding.writable),
            CheckedExprKind::Field { base, .. }
            | CheckedExprKind::Swizzle { base, .. }
            | CheckedExprKind::Index { base, .. } => self.is_assignable(*base),
            // An expression that already failed reports once, at its own site.
            CheckedExprKind::Invalid => true,
            _ => false,
        }
    }

    /// Whether every path through `body` returns.
    ///
    /// A `while` never counts: its condition may be false on entry, so a
    /// function whose only `return` is inside one can still fall out.
    pub(crate) fn returns_on_every_path(&self, body: &[CheckedStmtId]) -> bool {
        body.iter().any(|&id| match self.module.stmt(id) {
            CheckedStmt::Return(_) => true,
            CheckedStmt::If {
                then,
                otherwise: Some(otherwise),
                ..
            } => self.returns_on_every_path(then) && self.returns_on_every_path(otherwise),
            _ => false,
        })
    }
}

/// How a type is named in a diagnostic.
pub(crate) fn describe(ty: &Type) -> String {
    match ty {
        Type::Void => "Void".to_owned(),
        Type::Scalar(scalar) => scalar_name(*scalar).to_owned(),
        Type::Vector(vector) => format!("{}{}", scalar_name(vector.scalar), vector.width),
        Type::Matrix(matrix) => format!("Float{}x{}", matrix.columns, matrix.rows),
        Type::StructRef(name) => name.clone(),
        Type::Texture(_) => "a texture".to_owned(),
        Type::Sampler(_) => "a sampler".to_owned(),
        Type::RuntimeArray(element) => format!("[{}]", describe(element)),
    }
}

/// How a scalar is written in KSL.
fn scalar_name(scalar: ScalarType) -> &'static str {
    match scalar {
        ScalarType::Bool => "Bool",
        ScalarType::Int => "Int",
        ScalarType::Uint => "UInt",
        ScalarType::Float => "Float",
    }
}
