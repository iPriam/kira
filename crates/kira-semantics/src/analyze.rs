//! The semantic analyzer: turns a syntax tree into a typed [`HirProgram`].
//!
//! Analysis is a total function: it always produces a program plus a list of
//! diagnostics and never bails on the first error. Unresolved names and type
//! mismatches become [`HirExpr::Error`] nodes (type `Error`), which the type
//! lattice treats as compatible everywhere so one mistake does not cascade.

use std::collections::HashMap;

use kira_core::Interner;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_semantics_model::hir::{FuncId, HirFunction, HirLocal, HirProgram, LocalId};
use kira_semantics_model::{StructId, Type};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{ExprId, Function, Item};

/// The result of analyzing one program.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The typed program (always produced, possibly containing error nodes).
    pub program: HirProgram,
    /// Diagnostics discovered during analysis.
    pub diagnostics: Vec<Diagnostic>,
}

/// One declared function plus the struct it is a method of, if any.
#[derive(Clone, Copy)]
pub(crate) struct Callable<'a> {
    /// The struct whose method this is; `None` for a free function.
    pub(crate) receiver: Option<StructId>,
    /// The declaration as written.
    pub(crate) function: &'a Function,
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
    /// The struct this body is a method of, when it is one.
    ///
    /// A method's body may name a field bare — `return value + step` rather
    /// than `self.step` — so a name that resolves to no local is tried against
    /// this struct's fields before it is called undefined.
    pub(crate) receiver: Option<StructId>,
    /// How many loops enclose the statement being analyzed.
    ///
    /// A `break`/`continue` at depth zero has no loop to act on and is
    /// reported; every one that survives analysis therefore has a target.
    pub(crate) loop_depth: u32,
}

impl FnCtx {
    pub(crate) fn new(return_type: Type) -> Self {
        Self {
            locals: Vec::new(),
            scopes: vec![HashMap::new()],
            return_type,
            receiver: None,
            loop_depth: 0,
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

    /// Declares a local slot bound to no name, returning it.
    ///
    /// A desugaring needs storage the source never named — a `for` loop's
    /// cursor and limit. Binding it into no scope is what makes it
    /// unreachable: user code cannot read it, write it, or shadow it, whatever
    /// it spells its own variables, because name resolution only ever consults
    /// the scope stack.
    pub(crate) fn declare_hidden(&mut self, ty: Type, mutable: bool) -> LocalId {
        let id = LocalId(self.locals.len() as u32);
        self.locals.push(HirLocal {
            name: String::new(),
            ty,
            mutable,
        });
        id
    }

    /// Whether `local` may be reassigned.
    pub(crate) fn is_mutable(&self, local: LocalId) -> bool {
        self.locals[local.0 as usize].mutable
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
        let callables = self.callables();
        self.collect_signatures(&callables);
        self.check_main();
        // Bodies are analyzed in the same order the signatures were collected,
        // which is what makes a `FuncId` index both.
        for (index, callable) in callables.iter().enumerate() {
            let hir_function = self.analyze_function(FuncId(index as u32), callable);
            self.program.functions.push(hir_function);
        }
        Analysis {
            program: self.program,
            diagnostics: self.diagnostics,
        }
    }

    /// Every function the program declares, in one stable order: a free
    /// function where it was written, and a struct's methods where the struct
    /// was.
    ///
    /// A method is an ordinary function that happens to have a receiver, so it
    /// takes a slot in the same table. Everything downstream of analysis — the
    /// IR, both compilers, the hybrid manifest — sees a flat list of functions
    /// and never learns that some of them were written inside a struct.
    fn callables(&self) -> Vec<Callable<'a>> {
        let mut callables = Vec::new();
        for item in &self.tree.items {
            match item {
                Item::Function(function) => callables.push(Callable {
                    receiver: None,
                    function,
                }),
                Item::Struct(declaration) => {
                    let owner = self
                        .program
                        .structs
                        .lookup(self.interner.resolve(declaration.name));
                    for method in &declaration.methods {
                        callables.push(Callable {
                            receiver: owner,
                            function: method,
                        });
                    }
                }
                Item::Unsupported(_) => {}
            }
        }
        callables
    }

    fn collect_signatures(&mut self, callables: &[Callable<'a>]) {
        let mut main_seen = false;
        for callable in callables {
            let function = callable.function;
            let name = self.callable_name(*callable);
            // A method's receiver is parameter 0, so its signature carries the
            // struct type ahead of what was written.
            let mut params: Vec<Type> = callable.receiver.map(Type::Struct).into_iter().collect();
            params.extend(
                function
                    .params
                    .iter()
                    .map(|param| self.resolve_type(param.ty.name, param.ty.span)),
            );
            let return_type = match &function.return_type {
                Some(type_ref) => self.resolve_type(type_ref.name, type_ref.span),
                None => Type::Void,
            };
            let id = FuncId(self.sigs.len() as u32);
            if self.sig_index.contains_key(&name) {
                self.emit(
                    function.name_span,
                    "KSEM003",
                    match callable.receiver {
                        Some(_) => format!("`{name}` is already defined"),
                        None => format!("function `{name}` is already defined"),
                    },
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

    /// The name a callable is known by.
    ///
    /// A method is qualified with its struct (`Point.sum`), which is what keeps
    /// two structs' methods of the same name apart and keeps a method from
    /// colliding with a free function — `.` cannot appear in an identifier, so
    /// no user name can collide with a qualified one.
    pub(crate) fn callable_name(&self, callable: Callable<'_>) -> String {
        let written = self.interner.resolve(callable.function.name);
        match callable.receiver {
            Some(id) => format!(
                "{}.{written}",
                self.program.structs.type_name(Type::Struct(id))
            ),
            None => written.to_owned(),
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

    fn analyze_function(&mut self, id: FuncId, callable: &Callable<'a>) -> HirFunction {
        let function = callable.function;
        let sig_return = self.sigs[id.0 as usize].return_type;
        let mut ctx = FnCtx::new(sig_return);
        // A method's receiver is local 0, named `self`. It is immutable: a
        // method receives a copy like any other by-value parameter, so writing
        // to it would change nothing the caller could see, and letting it look
        // like it might would be worse than refusing.
        if let Some(owner) = callable.receiver {
            ctx.declare("self", Type::Struct(owner), false);
            ctx.receiver = Some(owner);
        }
        // Parameters become the next locals.
        for param in &function.params {
            let ty = self.resolve_type(param.ty.name, param.ty.span);
            let name = self.interner.resolve(param.name).to_owned();
            ctx.declare(&name, ty, false);
        }
        let param_count = function.params.len() as u32 + u32::from(callable.receiver.is_some());
        let body = self.analyze_block(&mut ctx, &function.body);
        // Definite-return check: a non-Void function must return on every
        // control path (the reference rejects this too). `Error` returns are
        // skipped to avoid cascading on an already-broken signature.
        if sig_return != Type::Void
            && sig_return != Type::Error
            && !self.body_definitely_returns(&body)
        {
            let name = self.callable_name(*callable);
            self.emit(
                function.name_span,
                "KSEM033",
                format!("`{name}` may finish without returning a value"),
            );
        }
        HirFunction {
            name: self.callable_name(*callable),
            param_count,
            return_type: sig_return,
            locals: ctx.locals,
            body,
            is_main: function.is_main,
            execution: function.execution,
            name_span: function.name_span,
        }
    }

    /// Whether `base_ty` has a field named `name`, reporting nothing.
    ///
    /// For a diagnostic that needs to know, not for resolving one.
    pub(crate) fn resolve_field_quietly(&self, base_ty: Type, name: &str) -> bool {
        matches!(base_ty, Type::Struct(id)
            if self
                .program
                .structs
                .get(id)
                .is_some_and(|def| def.field_index(name).is_some()))
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
