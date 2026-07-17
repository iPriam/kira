//! The semantic analyzer: turns a syntax tree into a typed [`HirProgram`].
//!
//! Analysis is a total function: it always produces a program plus a list of
//! diagnostics and never bails on the first error. Unresolved names and type
//! mismatches become [`HirExpr::Error`] nodes (type `Error`), which the type
//! lattice treats as compatible everywhere so one mistake does not cascade.

use std::collections::HashMap;

use kira_core::Interner;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_semantics_model::Type;
use kira_semantics_model::hir::{
    FuncId, HirFunction, HirLocal, HirPlace, HirProgram, HirStmt, HirStmtId, LocalId,
};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Block, Expr, ExprId, Function, Item, Stmt};

/// The result of analyzing one program.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The typed program (always produced, possibly containing error nodes).
    pub program: HirProgram,
    /// Diagnostics discovered during analysis.
    pub diagnostics: Vec<Diagnostic>,
}

/// The signature of a user function, resolved before bodies are checked so
/// calls can be type-checked regardless of declaration order.
struct FuncSig {
    name: String,
    params: Vec<Type>,
    return_type: Type,
    name_span: Span,
    is_main: bool,
}

/// Analyzes a parsed program.
pub fn analyze(source: SourceId, tree: &SyntaxTree, interner: &Interner) -> Analysis {
    Analyzer::new(source, tree, interner).run()
}

pub(crate) struct Analyzer<'a> {
    pub(crate) source: SourceId,
    pub(crate) tree: &'a SyntaxTree,
    pub(crate) interner: &'a Interner,
    sigs: Vec<FuncSig>,
    sig_index: HashMap<String, FuncId>,
    /// Each declared struct's field defaults, as written, indexed by
    /// [`kira_semantics_model::StructId`] and then by field index.
    ///
    /// Kept beside the table rather than in it: a default is unanalyzed syntax,
    /// and the table is a *model* type that carries no syntax. A construction
    /// site analyzes the default it needs, so a default that is never needed is
    /// never analyzed, and each use gets its own diagnostics.
    pub(crate) struct_defaults: Vec<Vec<Option<ExprId>>>,
    pub(crate) program: HirProgram,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

/// Per-function analysis state: the growing local table and the lexical scope
/// stack mapping names to slots.
pub(crate) struct FnCtx {
    pub(crate) locals: Vec<HirLocal>,
    pub(crate) scopes: Vec<HashMap<String, LocalId>>,
    pub(crate) return_type: Type,
}

impl FnCtx {
    pub(crate) fn new(return_type: Type) -> Self {
        Self {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            return_type,
        }
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Declares a new local in the innermost scope, returning its slot.
    pub(crate) fn declare(&mut self, name: &str, ty: Type, mutable: bool) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: name.to_owned(),
            ty,
            mutable,
        });
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_owned(), id);
        }
        id
    }

    /// Resolves a name to a local slot, searching innermost scope outward.
    pub(crate) fn resolve(&self, name: &str) -> Option<LocalId> {
        self.scopes
            .iter()
            .rev()
            .find_map(|scope| scope.get(name).copied())
    }

    pub(crate) fn local_type(&self, local: LocalId) -> Type {
        self.locals[local.0 as usize].ty
    }
}

impl<'a> Analyzer<'a> {
    fn new(source: SourceId, tree: &'a SyntaxTree, interner: &'a Interner) -> Self {
        Self {
            source,
            tree,
            interner,
            sigs: Vec::new(),
            sig_index: HashMap::new(),
            struct_defaults: Vec::new(),
            program: HirProgram::default(),
            diagnostics: Vec::new(),
        }
    }

    fn run(mut self) -> Analysis {
        self.collect_structs();
        self.collect_signatures();
        self.check_main();
        let functions: Vec<&Function> = self
            .tree
            .items
            .iter()
            .filter_map(|item| match item {
                Item::Function(function) => Some(function),
                Item::Struct(_) | Item::Unsupported(_) => None,
            })
            .collect();
        // Analyze bodies in declaration order.
        for (index, function) in functions.iter().enumerate() {
            let hir_function = self.analyze_function(FuncId(index as u32), function);
            self.program.functions.push(hir_function);
        }
        Analysis {
            program: self.program,
            diagnostics: self.diagnostics,
        }
    }

    fn collect_signatures(&mut self) {
        let mut main_seen = false;
        for item in &self.tree.items {
            let Item::Function(function) = item else {
                continue;
            };
            let name = self.interner.resolve(function.name).to_owned();
            let params = function
                .params
                .iter()
                .map(|param| self.resolve_type(param.ty.name, param.ty.span))
                .collect();
            let return_type = match &function.return_type {
                Some(type_ref) => self.resolve_type(type_ref.name, type_ref.span),
                None => Type::Void,
            };
            let id = FuncId(self.sigs.len() as u32);
            if self.sig_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM003",
                    format!("function `{name}` is already defined"),
                );
            } else {
                self.sig_index.insert(name.clone(), id);
            }
            let is_main = function.is_main;
            if is_main && main_seen {
                self.emit(
                    function.name_span,
                    "KSEM010",
                    "a program may declare only one `@Main` function",
                );
            }
            main_seen = main_seen || is_main;
            self.sigs.push(FuncSig {
                name,
                params,
                return_type,
                name_span: function.name_span,
                is_main,
            });
        }
    }

    fn check_main(&mut self) {
        // Snapshot the entrypoint's identity before emitting, so the
        // immutable borrow of `self.sigs` does not overlap `self.emit`.
        let main = self
            .sigs
            .iter()
            .find(|sig| sig.is_main)
            .map(|sig| (sig.name.clone(), sig.params.is_empty(), sig.name_span));
        match main {
            None => {
                self.emit(
                    Span::new(0, 0),
                    "KSEM011",
                    "program has no `@Main` function to run",
                );
            }
            Some((name, no_params, name_span)) => {
                if !no_params {
                    self.emit(name_span, "KSEM012", "`@Main` must take no parameters");
                }
                self.program.main = self.sig_index.get(&name).copied();
            }
        }
    }

    fn analyze_function(&mut self, id: FuncId, function: &Function) -> HirFunction {
        let sig_return = self.sigs[id.0 as usize].return_type;
        let mut ctx = FnCtx::new(sig_return);
        // Parameters become the first locals.
        for param in &function.params {
            let ty = self.resolve_type(param.ty.name, param.ty.span);
            let name = self.interner.resolve(param.name).to_owned();
            ctx.declare(&name, ty, false);
        }
        let param_count = function.params.len() as u32;
        let body = self.analyze_block(&mut ctx, &function.body);
        // Definite-return check: a non-Void function must return on every
        // control path (the reference rejects this too). `Error` returns are
        // skipped to avoid cascading on an already-broken signature.
        if sig_return != Type::Void
            && sig_return != Type::Error
            && !self.body_definitely_returns(&body)
        {
            let name = self.interner.resolve(function.name).to_owned();
            self.emit(
                function.name_span,
                "KSEM033",
                format!("function `{name}` may finish without returning a value"),
            );
        }
        HirFunction {
            name: self.interner.resolve(function.name).to_owned(),
            param_count,
            return_type: sig_return,
            locals: ctx.locals,
            body,
            is_main: function.is_main,
            execution: function.execution,
            name_span: function.name_span,
        }
    }

    /// Whether a statement list is guaranteed to execute a `return`.
    ///
    /// A list definitely returns when any of its statements does (everything
    /// after that statement is unreachable). An `if` definitely returns only
    /// when *both* arms do; a `while` never counts because its body may run
    /// zero times.
    fn body_definitely_returns(&self, stmts: &[HirStmtId]) -> bool {
        stmts.iter().any(|&id| self.stmt_definitely_returns(id))
    }

    fn stmt_definitely_returns(&self, id: HirStmtId) -> bool {
        match self.program.stmt(id) {
            HirStmt::Return { .. } => true,
            HirStmt::If {
                then_body,
                else_body,
                ..
            } => {
                // An empty else (no `else` written) can fall through, and
                // `body_definitely_returns` is false for an empty list.
                self.body_definitely_returns(then_body) && self.body_definitely_returns(else_body)
            }
            _ => false,
        }
    }

    pub(crate) fn analyze_block(&mut self, ctx: &mut FnCtx, block: &Block) -> Vec<HirStmtId> {
        ctx.push_scope();
        let mut stmts = Vec::with_capacity(block.stmts.len());
        for &stmt_id in &block.stmts {
            if let Some(hir) = self.analyze_stmt(ctx, stmt_id) {
                stmts.push(hir);
            }
        }
        ctx.pop_scope();
        stmts
    }

    fn analyze_stmt(
        &mut self,
        ctx: &mut FnCtx,
        stmt_id: kira_syntax_model::ast::StmtId,
    ) -> Option<HirStmtId> {
        match self.tree.stmt(stmt_id).clone() {
            Stmt::Let {
                name,
                name_span,
                mutable,
                ty,
                init,
                ..
            } => {
                let value = self.analyze_expr(ctx, init);
                let value_ty = self.program.expr(value).type_of();
                let declared = ty.map(|type_ref| self.resolve_type(type_ref.name, type_ref.span));
                let local_ty = match declared {
                    Some(annotation) => {
                        if !value_ty.assignable_to(annotation) {
                            self.emit(
                                name_span,
                                "KSEM020",
                                format!(
                                    "binding annotated `{}` cannot hold a value of type `{}`",
                                    self.type_name(annotation),
                                    self.type_name(value_ty)
                                ),
                            );
                        }
                        annotation
                    }
                    None => value_ty,
                };
                let name = self.interner.resolve(name).to_owned();
                let local = ctx.declare(&name, local_ty, mutable);
                Some(
                    self.program
                        .stmts
                        .alloc(HirStmt::Let { local, init: value }),
                )
            }
            Stmt::Assign { target, value, .. } => {
                let value_expr = self.analyze_expr(ctx, value);
                let value_ty = self.program.expr(value_expr).type_of();
                let target_span = self.tree.expr(target).span();
                let (place, place_ty) = self.resolve_place(ctx, target)?;
                if !value_ty.assignable_to(place_ty) {
                    self.emit(
                        target_span,
                        "KSEM022",
                        format!(
                            "cannot assign a value of type `{}` to a place of type `{}`",
                            self.type_name(value_ty),
                            self.type_name(place_ty)
                        ),
                    );
                }
                Some(self.program.stmts.alloc(HirStmt::Assign {
                    place,
                    value: value_expr,
                }))
            }
            Stmt::Return { value, span } => {
                let hir_value = value.map(|expr| self.analyze_expr(ctx, expr));
                self.check_return(ctx, hir_value, span);
                Some(
                    self.program
                        .stmts
                        .alloc(HirStmt::Return { value: hir_value }),
                )
            }
            Stmt::Expr { expr, .. } => {
                let hir = self.analyze_expr(ctx, expr);
                Some(self.program.stmts.alloc(HirStmt::Expr { expr: hir }))
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let cond_expr = self.analyze_condition(ctx, cond);
                let then_body = self.analyze_block(ctx, &then_block);
                let else_body = match &else_block {
                    Some(block) => self.analyze_block(ctx, block),
                    None => Vec::new(),
                };
                Some(self.program.stmts.alloc(HirStmt::If {
                    cond: cond_expr,
                    then_body,
                    else_body,
                }))
            }
            Stmt::While { cond, body, .. } => {
                let cond_expr = self.analyze_condition(ctx, cond);
                let loop_body = self.analyze_block(ctx, &body);
                Some(self.program.stmts.alloc(HirStmt::While {
                    cond: cond_expr,
                    body: loop_body,
                }))
            }
            Stmt::Error { .. } => None,
        }
    }

    /// Resolves an assignment target into a [`HirPlace`] plus the type stored
    /// there, or `None` when the target does not name a writable place.
    ///
    /// Every step must be writable, not just the last: a struct is a value, so
    /// writing `b.size.x` rewrites the `size` field's contents in place. A
    /// `let` anywhere along the path is what makes that illegal.
    fn resolve_place(&mut self, ctx: &mut FnCtx, target: ExprId) -> Option<(HirPlace, Type)> {
        match self.tree.expr(target).clone() {
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                let Some(local) = ctx.resolve(&name) else {
                    self.emit(
                        span,
                        "KSEM023",
                        format!("cannot assign to undefined name `{name}`"),
                    );
                    return None;
                };
                if !ctx.locals[local.0 as usize].mutable {
                    self.emit(
                        span,
                        "KSEM021",
                        format!(
                            "cannot assign to immutable binding `{name}` (declare it with `var`)"
                        ),
                    );
                    return None;
                }
                Some((
                    HirPlace {
                        local,
                        path: Vec::new(),
                    },
                    ctx.local_type(local),
                ))
            }
            Expr::Field {
                base,
                field,
                field_span,
                ..
            } => {
                let (mut place, base_ty) = self.resolve_place(ctx, base)?;
                let field_name = self.interner.resolve(field).to_owned();
                let (index, field_ty) = self.resolve_field(base_ty, &field_name, field_span)?;
                let mutable = match base_ty {
                    Type::Struct(id) => self
                        .program
                        .structs
                        .get(id)
                        .and_then(|def| def.field(index))
                        .is_some_and(|def| def.mutable),
                    _ => false,
                };
                if !mutable {
                    self.emit(
                        field_span,
                        "KSEM024",
                        format!(
                            "cannot assign to immutable field `{field_name}` of `{}` \
                             (declare it with `var`)",
                            self.type_name(base_ty)
                        ),
                    );
                    return None;
                }
                place.path.push(index);
                Some((place, field_ty))
            }
            other => {
                self.emit(
                    other.span(),
                    "KSEM025",
                    "the left side of an assignment must be a variable or a field of one",
                );
                None
            }
        }
    }

    /// Resolves `name` as a field of `base_ty`, returning its index and type.
    ///
    /// A field of a non-struct is reported once here; an `Error` base stays
    /// silent, because whatever produced it already spoke.
    pub(crate) fn resolve_field(
        &mut self,
        base_ty: Type,
        name: &str,
        span: Span,
    ) -> Option<(u32, Type)> {
        let Type::Struct(id) = base_ty else {
            if base_ty != Type::Error {
                self.emit(
                    span,
                    "KSEM090",
                    format!(
                        "type `{}` has no fields, so it has no field `{name}`",
                        self.type_name(base_ty)
                    ),
                );
            }
            return None;
        };
        let resolved = self
            .program
            .structs
            .get(id)
            .and_then(|def| def.field_index(name).map(|index| (index, def)))
            .and_then(|(index, def)| def.field(index).map(|field| (index, field.ty)));
        if resolved.is_none() {
            self.emit(
                span,
                "KSEM091",
                format!("struct `{}` has no field `{name}`", self.type_name(base_ty)),
            );
        }
        resolved
    }

    fn check_return(
        &mut self,
        ctx: &FnCtx,
        value: Option<kira_semantics_model::hir::HirExprId>,
        span: Span,
    ) {
        let expected = ctx.return_type;
        match value {
            None => {
                if expected != Type::Void {
                    self.emit(
                        span,
                        "KSEM030",
                        format!(
                            "function must return a value of type `{}`",
                            self.type_name(expected)
                        ),
                    );
                }
            }
            Some(expr) => {
                let actual = self.program.expr(expr).type_of();
                if expected == Type::Void {
                    self.emit(span, "KSEM031", "a `Void` function cannot return a value");
                } else if !actual.assignable_to(expected) {
                    self.emit(
                        span,
                        "KSEM032",
                        format!(
                            "returning `{}` from a function declared to return `{}`",
                            self.type_name(actual),
                            self.type_name(expected)
                        ),
                    );
                }
            }
        }
    }

    fn analyze_condition(
        &mut self,
        ctx: &mut FnCtx,
        expr: ExprId,
    ) -> kira_semantics_model::hir::HirExprId {
        let cond_span = self.tree.expr(expr).span();
        let hir = self.analyze_expr(ctx, expr);
        let ty = self.program.expr(hir).type_of();
        if ty != Type::Bool && ty != Type::Error {
            self.emit(
                cond_span,
                "KSEM040",
                format!("condition must be `Bool`, found `{}`", self.type_name(ty)),
            );
        }
        hir
    }

    /// Looks up a signature by name (for call resolution).
    pub(crate) fn lookup_function(&self, name: &str) -> Option<(FuncId, &[Type], Type)> {
        let id = *self.sig_index.get(name)?;
        let sig = &self.sigs[id.0 as usize];
        Some((id, &sig.params, sig.return_type))
    }

    /// Resolves a written type name to a builtin or a declared struct.
    pub(crate) fn resolve_type(&mut self, name: kira_core::Symbol, span: Span) -> Type {
        let text = self.interner.resolve(name).to_owned();
        if let Some(ty) = Type::from_name(&text) {
            return ty;
        }
        if let Some(id) = self.program.structs.lookup(&text) {
            return Type::Struct(id);
        }
        self.emit(
            span,
            "KSEM050",
            format!(
                "unknown type `{text}` (v0 supports Int, Float, Bool, String, Void, \
                 and declared structs)"
            ),
        );
        Type::Error
    }

    /// The spelling of `ty` for a diagnostic.
    ///
    /// Owned rather than borrowed on purpose: a struct's name lives in
    /// `self.program`, and holding a borrow of it across an `emit` — which
    /// needs `&mut self` — would not compile.
    pub(crate) fn type_name(&self, ty: Type) -> String {
        self.program.structs.type_name(ty).to_owned()
    }

    pub(crate) fn emit(&mut self, span: Span, code: &'static str, message: impl Into<String>) {
        let message = message.into();
        let file_span = FileSpan::new(self.source, span);
        let mut diagnostic = Diagnostic::single(
            Severity::Error,
            message.clone(),
            Label::primary(file_span, message),
        );
        diagnostic.code = Some(code);
        diagnostic.phase = Some("semantics");
        self.diagnostics.push(diagnostic);
    }
}
