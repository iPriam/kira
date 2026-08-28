//! Module-scope constants: `let Name = value` written as an item.
//!
//! One value per program, computed once at program start — before `@Main` runs
//! — and shared by every reader. The initializer is an ordinary runtime
//! expression, so it is analyzed like a function body and lowered as one: each
//! constant becomes a synthesized zero-argument function plus a row in
//! [`HirProgram::constants`], and every read becomes a
//! [`HirExpr::ConstantGet`] of that row's global slot.
//!
//! # Evaluation order
//!
//! Each constant is evaluated after every constant it depends on, whatever
//! order the declarations were written in and whichever files they sit in. The
//! dependency relation is computed from syntax, conservatively: every name an
//! initializer mentions counts, and a mention of a function (or of a type with
//! defaulted members) pulls in every constant *that* declaration's expressions
//! mention, transitively through the whole call graph. Over-approximating only
//! ever moves a constant later, which is harmless; under-approximating would
//! read an uninitialized slot. A dependency cycle has no first value and is
//! refused ([`KSEM317`](#diagnostics)).
//!
//! # Namespace
//!
//! Constants share the value namespace with top-level functions: a constant
//! that collides with a function, a foreign callable, or another constant is
//! refused (KSEM316). Locals still shadow constants inside a body, exactly as
//! they shadow functions.
//!
//! [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
//! [`HirExpr::ConstantGet`]: kira_semantics_model::hir::HirExpr

use std::collections::{BTreeSet, HashMap, HashSet};

use kira_core::Names;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirConstant, HirExpr, HirExprId, HirFunction, HirStmt};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Block, ConstantDecl, Expr, ExprId, ForIterable, Item, Stmt, StmtId};

use crate::analyze::{Analyzer, Callable, FnCtx};

/// One collected module-scope constant, as the value namespace sees it.
///
/// Indexed in lockstep with [`HirProgram::constants`], so the position of an
/// entry here is the global slot a read loads from.
///
/// [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
pub(crate) struct ConstantEntry {
    /// The resolved type: declared when written, inferred otherwise.
    pub(crate) ty: Type,
    /// The declaring file, which gates visibility the way a function's does.
    pub(crate) source: SourceId,
    /// Span of the name token, for definition links.
    pub(crate) name_span: Span,
}

impl<'a> Analyzer<'a> {
    /// Collects every module-scope constant: names, types, evaluation order,
    /// and one synthesized initializer function per constant.
    ///
    /// Runs after signatures and foreign callables exist — an initializer may
    /// call anything a function body may — and before any body is analyzed, so
    /// a read in a body resolves against the finished table.
    pub(crate) fn collect_constants(&mut self, callables: &[Callable<'a>]) {
        let tree = self.tree;
        let mut decls: Vec<(SourceId, &ConstantDecl)> = Vec::new();
        for (source, item) in tree.items_with_source() {
            if let Item::Constant(declaration) = item {
                decls.push((source, declaration));
            }
        }
        if decls.is_empty() {
            return;
        }
        let decls = self.refuse_constant_name_clashes(decls);
        let order = self.constant_evaluation_order(callables, &decls);
        for index in order {
            let (source, declaration) = decls[index];
            self.analyze_constant(source, declaration, false);
        }
    }

    /// Drops every declaration whose name is already taken in the value
    /// namespace, reporting each drop, and returns the survivors in
    /// declaration order.
    fn refuse_constant_name_clashes(
        &mut self,
        decls: Vec<(SourceId, &'a ConstantDecl)>,
    ) -> Vec<(SourceId, &'a ConstantDecl)> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut kept = Vec::with_capacity(decls.len());
        for (source, declaration) in decls {
            let name = self.interner.resolve(declaration.name).to_owned();
            let taken_by_function =
                self.sig_index.contains_key(&name) || self.foreign_index.contains_key(&name);
            if taken_by_function {
                self.source = source;
                self.emit(
                    declaration.name_span,
                    "KSEM316",
                    format!(
                        "constant `{name}` collides with the function of the same name: \
                         constants and functions share one value namespace"
                    ),
                );
                continue;
            }
            if !seen.insert(name.clone()) {
                self.source = source;
                self.emit(
                    declaration.name_span,
                    "KSEM316",
                    format!("constant `{name}` is already defined"),
                );
                continue;
            }
            kept.push((source, declaration));
        }
        kept
    }

    /// The order the constants are evaluated in: each after everything it
    /// depends on, ties broken by declaration order.
    ///
    /// Members of a dependency cycle are refused (KSEM317) and appended at the
    /// end as error entries, so a read of one resolves to `Error` instead of
    /// cascading into an undefined name.
    fn constant_evaluation_order(
        &mut self,
        callables: &[Callable<'a>],
        decls: &[(SourceId, &ConstantDecl)],
    ) -> Vec<usize> {
        let mentions = declaration_mentions(self.tree, self.interner, callables);
        let names: Vec<String> = decls
            .iter()
            .map(|(_, declaration)| self.interner.resolve(declaration.name).to_owned())
            .collect();
        let index_of: HashMap<&str, usize> = names
            .iter()
            .enumerate()
            .map(|(index, name)| (name.as_str(), index))
            .collect();
        let deps: Vec<BTreeSet<usize>> = decls
            .iter()
            .enumerate()
            .map(|(index, (_, declaration))| {
                let mut reached = expression_mentions(self.tree, self.interner, declaration.value);
                close_over_declarations(&mentions, &mut reached);
                reached
                    .iter()
                    .filter_map(|name| index_of.get(name.as_str()).copied())
                    .filter(|&dep| dep != index || reached.contains(&names[index]))
                    .collect()
            })
            .collect();
        // Self-mention is a cycle of one; the filter above keeps the edge only
        // when the initializer genuinely reaches its own name.
        let mut placed: Vec<bool> = vec![false; decls.len()];
        let mut order = Vec::with_capacity(decls.len());
        loop {
            let next = (0..decls.len())
                .find(|&index| !placed[index] && deps[index].iter().all(|&dep| placed[dep]));
            match next {
                Some(index) => {
                    placed[index] = true;
                    order.push(index);
                }
                None => break,
            }
        }
        // Whatever is left sits on at least one cycle.
        let stuck: Vec<usize> = (0..decls.len()).filter(|&index| !placed[index]).collect();
        if !stuck.is_empty() {
            self.report_constant_cycle(&names, &deps, &placed, stuck[0]);
        }
        for index in &stuck {
            let (source, declaration) = decls[*index];
            self.analyze_constant(source, declaration, true);
        }
        order
    }

    /// Reports one dependency cycle, naming its members in walk order.
    fn report_constant_cycle(
        &mut self,
        names: &[String],
        deps: &[BTreeSet<usize>],
        placed: &[bool],
        start: usize,
    ) {
        // Walk unplaced dependency edges until a node repeats; the repeat and
        // everything after it is a cycle.
        let mut path = vec![start];
        let mut at = start;
        let cycle = loop {
            let Some(&next) = deps[at].iter().find(|&&dep| !placed[dep]) else {
                // Every stuck node keeps at least one unplaced edge, so the
                // walk cannot dead-end; this arm exists so a logic error here
                // degrades to naming the path walked rather than panicking.
                break path.clone();
            };
            if let Some(position) = path.iter().position(|&seen| seen == next) {
                break path[position..].to_vec();
            }
            path.push(next);
            at = next;
        };
        let spelled: Vec<String> = cycle
            .iter()
            .chain(cycle.first())
            .map(|&index| format!("`{}`", names[index]))
            .collect();
        // Attributed to the first cycle member's declaration; `self.source` is
        // set by the caller before each constant is analyzed, so it is set
        // here explicitly too.
        let member = cycle[0];
        let span = self.constant_decl_span(&names[member]);
        if let Some((source, span)) = span {
            self.source = source;
            self.emit(
                span,
                "KSEM317",
                format!(
                    "module constants form a dependency cycle: {}; no member has a value to \
                     start from",
                    spelled.join(" -> ")
                ),
            );
        }
    }

    /// The declaring file and name span of the constant declaration named
    /// `name`, from syntax.
    fn constant_decl_span(&self, name: &str) -> Option<(SourceId, Span)> {
        for (source, item) in self.tree.items_with_source() {
            if let Item::Constant(declaration) = item
                && self.interner.resolve(declaration.name) == name
            {
                return Some((source, declaration.name_span));
            }
        }
        None
    }

    /// Analyzes one constant's initializer and records it: a table entry, a
    /// row in [`HirProgram::constants`], and a synthesized zero-argument
    /// function computing the value.
    ///
    /// `cycle_member` marks a declaration on a refused dependency cycle: its
    /// initializer is not analyzed — it has no value to compute — and its
    /// entry carries the declared type (or `Error`), so reads of it do not
    /// cascade into undefined names.
    ///
    /// [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
    fn analyze_constant(
        &mut self,
        source: SourceId,
        declaration: &ConstantDecl,
        cycle_member: bool,
    ) {
        self.source = source;
        let name = self.interner.resolve(declaration.name).to_owned();
        let declared = declaration
            .declared_type
            .map(|type_ref| self.resolve_type_ref(type_ref));
        let mut ctx = FnCtx::new(declared.unwrap_or(Type::Void));
        let (ty, value) = if cycle_member {
            let ty = declared.unwrap_or(Type::Error);
            (ty, self.program.exprs.alloc(HirExpr::Error))
        } else {
            let value = self.analyze_expr_expecting(&mut ctx, declaration.value, declared);
            let value_ty = self.program.expr(value).type_of();
            let ty = match declared {
                Some(annotation) => {
                    if !self.admits(value_ty, annotation) {
                        self.emit(
                            declaration.name_span,
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
            (ty, self.coerce_into(value, ty))
        };
        // A value that runs a user `Drop` body has one owner and is never
        // copied; a constant is shared by every reader, and every read copies.
        // The two cannot both hold, so the declaration is refused.
        if self.program.types.runs_user_drop(ty) {
            self.emit(
                declaration.name_span,
                "KSEM318",
                format!(
                    "a module constant cannot hold `{}`: its value is shared by every reader, \
                     and a value with a `Drop` body has exactly one owner",
                    self.type_name(ty)
                ),
            );
        }
        let init = self.constant_init_function(&mut ctx, &name, ty, value, declaration.name_span);
        self.constant_index
            .insert(name.clone(), self.constants.len() as u32);
        self.constants.push(ConstantEntry {
            ty,
            source,
            name_span: declaration.name_span,
        });
        self.program.constants.push(HirConstant { name, ty, init });
    }

    /// Builds and registers the synthesized zero-argument function whose body
    /// computes one constant's value.
    ///
    /// The value is bound to a hidden local before the return so any deferred
    /// statements the initializer produced run between computing and
    /// returning, exactly as they do around an ordinary statement.
    fn constant_init_function(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        ty: Type,
        value: HirExprId,
        name_span: Span,
    ) -> kira_semantics_model::hir::FuncId {
        let pending = ctx.take_pending_stmts();
        let deferred = ctx.take_deferred_stmts();
        let slot = ctx.declare_hidden(ty, false);
        let bind = self.program.stmts.alloc(HirStmt::Let {
            local: slot,
            init: value,
        });
        let read = self.program.exprs.alloc(HirExpr::Local { local: slot, ty });
        let give = self
            .program
            .stmts
            .alloc(HirStmt::Return { value: Some(read) });
        let mut body = pending;
        body.push(bind);
        body.extend(deferred);
        body.push(give);
        let function = HirFunction {
            // `$` cannot appear in an identifier, so this collides with no
            // user symbol — and not with a construct's `Name$init` either.
            name: format!("{name}$constant"),
            param_count: 0,
            return_type: ty,
            locals: std::mem::take(&mut ctx.locals),
            body,
            is_main: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span,
        };
        let id = self.reserve_synth();
        self.fill_synth(id, function);
        id
    }

    /// The type of the module constant named `name`, when one is visible from
    /// the file being analyzed.
    pub(crate) fn constant_type(&self, name: &str) -> Option<Type> {
        let index = *self.constant_index.get(name)?;
        let entry = &self.constants[index as usize];
        self.imports
            .sees(self.source, entry.source)
            .then_some(entry.ty)
    }

    /// The read of a module constant named `name`, when one is visible from
    /// the file being analyzed.
    ///
    /// Visibility is the function rule: a constant declared in another package
    /// is nameable only from a file that imports that package (see
    /// [`crate::imports::ImportTable::sees`]).
    pub(crate) fn constant_read(&mut self, name: &str, span: Span) -> Option<HirExprId> {
        let index = *self.constant_index.get(name)?;
        let entry = &self.constants[index as usize];
        if !self.imports.sees(self.source, entry.source) {
            return None;
        }
        let (ty, definition) = (entry.ty, FileSpan::new(entry.source, entry.name_span));
        self.link(span, definition);
        Some(self.program.exprs.alloc(HirExpr::ConstantGet {
            constant: index,
            ty,
        }))
    }
}

/// The mention set of every declaration whose expressions can hide a constant
/// read, keyed by the name a mention would spell.
///
/// Function bodies and parameter defaults land under the function's bare name;
/// a type's field, member, and variant-payload defaults land under the type's
/// name. Overloads and same-named methods union into one set, which only
/// over-approximates.
fn declaration_mentions(
    tree: &SyntaxTree,
    interner: &Names,
    callables: &[Callable<'_>],
) -> HashMap<String, HashSet<String>> {
    let mut mentions: HashMap<String, HashSet<String>> = HashMap::new();
    let mut note = |name: String, found: HashSet<String>| {
        mentions.entry(name).or_default().extend(found);
    };
    for callable in callables {
        let function = callable.function;
        let name = interner.resolve(function.name).to_owned();
        let mut found = block_mentions(tree, interner, &function.body);
        for param in &function.params {
            if let Some(default) = param.default {
                found.extend(expression_mentions(tree, interner, default));
            }
        }
        note(name, found);
    }
    for (_, item) in tree.items_with_source() {
        match item {
            Item::Struct(declaration) => {
                let mut found = HashSet::new();
                for field in &declaration.fields {
                    if let Some(default) = field.default {
                        found.extend(expression_mentions(tree, interner, default));
                    }
                }
                note(interner.resolve(declaration.name).to_owned(), found);
            }
            Item::Class(declaration) => {
                let mut found = HashSet::new();
                for field in &declaration.fields {
                    if let Some(default) = field.default {
                        found.extend(expression_mentions(tree, interner, default));
                    }
                }
                for field in &declaration.overrides {
                    found.extend(expression_mentions(tree, interner, field.default));
                }
                note(interner.resolve(declaration.name).to_owned(), found);
            }
            Item::Enum(declaration) => {
                let mut found = HashSet::new();
                for variant in &declaration.variants {
                    if let Some(default) = variant.default {
                        found.extend(expression_mentions(tree, interner, default));
                    }
                }
                note(interner.resolve(declaration.name).to_owned(), found);
            }
            Item::Construct(declaration) => {
                let mut found = HashSet::new();
                for field in &declaration.fields {
                    if let Some(default) = field.default {
                        found.extend(expression_mentions(tree, interner, default));
                    }
                }
                note(interner.resolve(declaration.name).to_owned(), found);
            }
            // Extend modifiers are not ordinary callables, so their bodies are
            // walked here; a constant initializer can reach one through a
            // family value's method call.
            Item::Extend(declaration) => {
                for method in &declaration.methods {
                    note(
                        interner.resolve(method.name).to_owned(),
                        block_mentions(tree, interner, &method.body),
                    );
                }
            }
            Item::Function(_)
            | Item::TypeAlias(_)
            | Item::Constant(_)
            | Item::Import(_)
            | Item::Trait(_)
            | Item::Unsupported(_) => {}
        }
    }
    mentions
}

/// Expands `reached` to its transitive closure over the declaration mention
/// map: any reached name that has a mention set contributes that set.
fn close_over_declarations(
    mentions: &HashMap<String, HashSet<String>>,
    reached: &mut HashSet<String>,
) {
    let mut queue: Vec<String> = reached.iter().cloned().collect();
    while let Some(name) = queue.pop() {
        let Some(found) = mentions.get(&name) else {
            continue;
        };
        for mention in found {
            if reached.insert(mention.clone()) {
                queue.push(mention.clone());
            }
        }
    }
}

/// Every name one expression tree mentions, by a conservative syntactic walk.
fn expression_mentions(tree: &SyntaxTree, interner: &Names, expr: ExprId) -> HashSet<String> {
    let mut scan = MentionScan {
        tree,
        interner,
        found: HashSet::new(),
    };
    scan.expr(expr);
    scan.found
}

/// Every name one block mentions, by the same walk.
fn block_mentions(tree: &SyntaxTree, interner: &Names, block: &Block) -> HashSet<String> {
    let mut scan = MentionScan {
        tree,
        interner,
        found: HashSet::new(),
    };
    scan.block(block);
    scan.found
}

/// A syntactic walk collecting every identifier an expression tree mentions:
/// bare names, callee names, and method names alike.
///
/// By name, not by resolution — resolution has not run yet, and the answer
/// only orders evaluation, so counting a shadowed or unrelated name merely
/// moves a constant later than it strictly had to be.
struct MentionScan<'t> {
    tree: &'t SyntaxTree,
    interner: &'t Names,
    found: HashSet<String>,
}

impl MentionScan<'_> {
    fn note(&mut self, symbol: kira_core::Symbol) {
        self.found.insert(self.interner.resolve(symbol).to_owned());
    }

    fn block(&mut self, block: &Block) {
        for &stmt in &block.stmts {
            self.stmt(stmt);
        }
    }

    fn stmt(&mut self, id: StmtId) {
        match self.tree.stmt(id).clone() {
            Stmt::Let { init, .. } => self.expr(init),
            Stmt::Assign { target, value, .. } => {
                self.expr(target);
                self.expr(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(value) = value {
                    self.expr(value);
                }
            }
            Stmt::Expr { expr, .. } => self.expr(expr),
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(&then_block);
                if let Some(block) = &else_block {
                    self.block(block);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(&body);
            }
            Stmt::For { iterable, body, .. } => {
                match iterable {
                    ForIterable::Range { start, end } => {
                        self.expr(start);
                        self.expr(end);
                    }
                    ForIterable::Each { array } => self.expr(array),
                }
                self.block(&body);
            }
            Stmt::Match { subject, arms, .. } => {
                self.expr(subject);
                for arm in &arms {
                    self.block(&arm.body);
                }
            }
            Stmt::Attempt { body, handlers, .. } => {
                self.block(&body);
                for handler in &handlers {
                    self.block(&handler.body);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Error { .. } => {}
        }
    }

    fn expr(&mut self, id: ExprId) {
        match self.tree.expr(id).clone() {
            Expr::Name { symbol, .. } => self.note(symbol),
            Expr::Closure { body, .. } => self.block(&body),
            Expr::Unary { operand, .. } => self.expr(operand),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Conditional {
                cond,
                then,
                otherwise,
                ..
            } => {
                self.expr(cond);
                self.expr(then);
                self.expr(otherwise);
            }
            Expr::Call {
                callee,
                args,
                children,
                trailing_closure,
                ..
            } => {
                self.note(callee);
                for arg in &args {
                    self.expr(arg.value);
                }
                for &child in &children {
                    self.expr(child);
                }
                if let Some(trailing) = trailing_closure {
                    self.expr(trailing.closure);
                }
            }
            Expr::StructLit { name, fields, .. } => {
                self.note(name);
                for field in &fields {
                    self.expr(field.value);
                }
            }
            Expr::MethodCall {
                receiver,
                method,
                args,
                children,
                ..
            } => {
                self.note(method);
                self.expr(receiver);
                for arg in &args {
                    self.expr(arg.value);
                }
                for &child in &children {
                    self.expr(child);
                }
            }
            Expr::Field { base, .. } => self.expr(base),
            Expr::ArrayLit { elements, .. } => {
                for &element in &elements {
                    self.expr(element);
                }
            }
            Expr::Index { base, index, .. } => {
                self.expr(base);
                self.expr(index);
            }
            Expr::DotMember { args, .. } => {
                for &arg in args.iter().flatten() {
                    self.expr(arg);
                }
            }
            Expr::Try { value, .. } => self.expr(value),
            Expr::Ownership { operand, .. } => self.expr(operand),
            Expr::ContentFor { iterable, body, .. } => {
                self.expr(iterable);
                for &item in &body {
                    self.expr(item);
                }
            }
            Expr::Content {
                children, closure, ..
            } => {
                for &child in &children {
                    self.expr(child);
                }
                if let Some(closure) = closure {
                    self.expr(closure);
                }
            }
            Expr::ContentIf {
                cond,
                then_body,
                else_body,
                ..
            } => {
                self.expr(cond);
                for &item in then_body.iter().chain(else_body.iter()) {
                    self.expr(item);
                }
            }
            Expr::TaskSpawn { body, .. } => self.expr(body),
            Expr::Int { .. }
            | Expr::Float { .. }
            | Expr::Bool { .. }
            | Expr::Str { .. }
            | Expr::Error { .. } => {}
        }
    }
}
