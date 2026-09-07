//! Module-scope constants: `let Name = value` written as an item.
//!
//! One value per program, computed once at program start — before `@Main` runs
//! — and shared by every reader. The initializer is an ordinary runtime
//! expression, so it is analyzed like a function body and lowered as one: each
//! constant becomes a synthesized zero-argument function plus a row in
//! [`HirProgram::constants`], and every read becomes a
//! [`HirExpr::ConstantGet`] of that row's global slot.
//!
//! # Analysis order
//!
//! Initializers are analyzed on demand: reading a constant whose initializer
//! has not been analyzed analyzes it first, wherever the read sits — spelled
//! directly, or hidden behind a field default the initializer's construction
//! forces. A constant met again while its own initializer is still being
//! analyzed has no value to start from, and that resolution cycle is refused
//! ([`KSEM317`](#diagnostics)).
//!
//! # Evaluation order
//!
//! What order the slots are *filled in* at program start is decided after
//! every body has been analyzed, from the resolved HIR — see
//! [`crate::constant_order`]. Resolution above only has to see each direct
//! read's type; the runtime order additionally follows calls, and a name-level
//! guess at that call graph refused programs whose names merely collided.
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

use std::collections::HashSet;

use kira_semantics_model::Type;
use kira_semantics_model::hir::{FuncId, HirConstant, HirExpr, HirExprId, HirStmt};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::ast::{ConstantDecl, Item};

use crate::analyze::{Analyzer, FnCtx};

/// One collected module-scope constant, as the value namespace sees it.
///
/// Indexed in lockstep with [`HirProgram::constants`], so the position of an
/// entry here is the global slot a read loads from.
///
/// [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
pub(crate) struct ConstantEntry {
    /// The resolved type: declared when written, inferred otherwise, `Error`
    /// until the initializer has been analyzed.
    pub(crate) ty: Type,
    /// The declaring file, which gates visibility the way a function's does.
    pub(crate) source: SourceId,
    /// Span of the name token, for definition links.
    pub(crate) name_span: Span,
}

/// How far one constant's demand-driven initializer analysis has gone.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConstantProgress {
    /// Registered; the initializer has not been analyzed yet.
    Pending,
    /// The initializer is being analyzed right now. Reading the constant in
    /// this state is a resolution cycle.
    InProgress,
    /// The entry's type and initializer function are final.
    Done,
}

impl<'a> Analyzer<'a> {
    /// Collects every module-scope constant: names, slots, and one analyzed
    /// initializer per constant.
    ///
    /// Runs after signatures and foreign callables exist — an initializer may
    /// call anything a function body may — and before any body is analyzed, so
    /// a read in a body resolves against the finished table. Slots are handed
    /// out in declaration order; each initializer is analyzed on first demand,
    /// so a forward read works no matter which file it sits in.
    pub(crate) fn collect_constants(&mut self) {
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
        for (source, declaration) in decls {
            let name = self.interner.resolve(declaration.name).to_owned();
            let init = self.reserve_synth();
            self.constant_index
                .insert(name.clone(), self.constants.len() as u32);
            self.constants.push(ConstantEntry {
                ty: Type::Error,
                source,
                name_span: declaration.name_span,
            });
            self.constant_decls.push((source, declaration));
            self.constant_progress.push(ConstantProgress::Pending);
            self.program.constants.push(HirConstant {
                name,
                ty: Type::Error,
                init,
            });
        }
        for slot in 0..self.constants.len() as u32 {
            self.ensure_constant(slot);
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

    /// Analyzes the constant in `slot` unless that has already happened.
    ///
    /// Reading a constant this pass has not reached yet lands here through
    /// [`Analyzer::constant_type`] and [`Analyzer::constant_read`], which is
    /// what makes the analysis order the resolution order: whatever a
    /// visible initializer genuinely reads — directly, or through a field
    /// default its construction forces — is analyzed first. A slot met again
    /// while it is still on the stack has no value to start from and is
    /// refused.
    pub(crate) fn ensure_constant(&mut self, slot: u32) {
        match self.constant_progress[slot as usize] {
            ConstantProgress::Done => return,
            ConstantProgress::InProgress => {
                self.report_constant_cycle(slot);
                return;
            }
            ConstantProgress::Pending => {}
        }
        self.constant_progress[slot as usize] = ConstantProgress::InProgress;
        self.constant_stack.push(slot);
        let (source, declaration) = self.constant_decls[slot as usize];
        let saved_source = self.source;
        self.analyze_constant(slot, source, declaration);
        self.source = saved_source;
        self.constant_stack.pop();
        self.constant_progress[slot as usize] = ConstantProgress::Done;
    }

    /// Reports the resolution cycle that reading `slot` mid-analysis closes,
    /// naming its members in read order.
    fn report_constant_cycle(&mut self, slot: u32) {
        let members = match self.constant_stack.iter().position(|&on| on == slot) {
            Some(position) => &self.constant_stack[position..],
            None => return,
        };
        let spelled: Vec<String> = members
            .iter()
            .chain(members.first())
            .map(|&member| format!("`{}`", self.program.constants[member as usize].name))
            .collect();
        let entry = &self.constants[slot as usize];
        let (source, span) = (entry.source, entry.name_span);
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

    /// Analyzes one constant's initializer and records it: the entry's type,
    /// the row in [`HirProgram::constants`], and the synthesized zero-argument
    /// function computing the value.
    ///
    /// A member of a refused resolution cycle still lands here — the cycle was
    /// reported where the read closed it — and completes with whatever the
    /// analysis could give it, so reads of it do not cascade into undefined
    /// names.
    ///
    /// [`HirProgram::constants`]: kira_semantics_model::hir::HirProgram
    /// Analyzes one constant in its own file, leaving the current file as it
    /// was: a constant is analyzed on first read, from whatever file was
    /// reading it, and that file's own names must still resolve afterwards.
    fn analyze_constant(&mut self, slot: u32, source: SourceId, declaration: &ConstantDecl) {
        let here = self.source;
        self.analyze_constant_in_file(slot, source, declaration);
        self.source = here;
    }

    fn analyze_constant_in_file(
        &mut self,
        slot: u32,
        source: SourceId,
        declaration: &ConstantDecl,
    ) {
        self.source = source;
        let name = self.interner.resolve(declaration.name).to_owned();
        let declared = declaration
            .declared_type
            .map(|type_ref| self.resolve_type_ref(type_ref));
        let mut ctx = FnCtx::new(declared.unwrap_or(Type::Void));
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
        let value = self.coerce_into(value, ty);
        // A value that runs a user `Drop` body has one owner and is never
        // copied; a constant is shared by every reader, and every read copies.
        // The two cannot both hold, so the declaration is refused.
        if self.program.types.runs_user_drop(ty) {
            self.emit(
                declaration.name_span,
                "KSEM318",
                format!(
                    "a module constant cannot hold `{}`: its value is shared by every reader, \
                     and a `Drop` value has exactly one owner",
                    self.type_name(ty)
                ),
            );
        }
        let init = self.program.constants[slot as usize].init;
        self.fill_constant_init(init, &mut ctx, &name, ty, value, declaration.name_span);
        self.constants[slot as usize].ty = ty;
        self.program.constants[slot as usize].ty = ty;
    }

    /// Fills the reserved synthesized function whose body computes one
    /// constant's value.
    ///
    /// The value is bound to a hidden local before the return so any deferred
    /// statements the initializer produced run between computing and
    /// returning, exactly as they do around an ordinary statement.
    fn fill_constant_init(
        &mut self,
        id: FuncId,
        ctx: &mut FnCtx,
        name: &str,
        ty: Type,
        value: HirExprId,
        name_span: Span,
    ) {
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
        let function = kira_semantics_model::hir::HirFunction {
            // `$` cannot appear in an identifier, so this collides with no
            // user symbol — and not with a construct's `Name$init` either.
            name: format!("{name}$constant"),
            param_count: 0,
            return_type: ty,
            locals: std::mem::take(&mut ctx.locals),
            body,
            is_main: false,
            is_main_thread: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span,
            signature: kira_semantics_model::hir::CallableSignature::synthesized(&[], ty),
        };
        self.fill_synth(id, function);
    }

    /// The type of the module constant named `name`, when one is visible from
    /// the file being analyzed.
    pub(crate) fn constant_type(&mut self, name: &str) -> Option<Type> {
        let index = *self.constant_index.get(name)?;
        self.ensure_constant(index);
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
        self.ensure_constant(index);
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
