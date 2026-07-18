//! The semantic analyzer: turns a syntax tree into a typed [`HirProgram`].
//!
//! Analysis is a total function: it always produces a program plus a list of
//! diagnostics and never bails on the first error. Unresolved names and type
//! mismatches become [`HirExpr::Error`] nodes (type `Error`), which the type
//! lattice treats as compatible everywhere so one mistake does not cascade.

use std::collections::{BTreeSet, HashMap};

use kira_core::Interner;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_semantics_model::hir::{FuncId, HirFunction, HirProgram};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{ExprId, Function, Item};

mod scope;

pub(crate) use scope::FnCtx;

use crate::aliases::AliasTable;
use crate::build_kind::BuildKind;

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
    /// For a class method copied from an ancestor, the ancestor that wrote the
    /// body; `None` for a free function or a method written where it lives.
    ///
    /// This is what makes inheritance work without a vtable: the same body is
    /// registered once per class that inherits it, each time with `receiver`
    /// set to *that* class, so `self` is statically the concrete type.
    pub(crate) origin: Option<StructId>,
    /// The declaration as written.
    pub(crate) function: &'a Function,
    /// The file the declaration was written in.
    ///
    /// Carried so a diagnostic about this function points into the right file,
    /// and so its body resolves qualified names against *that* file's imports —
    /// which is the whole of file scoping.
    pub(crate) source: SourceId,
}

/// The signature of a user function, resolved before bodies are checked so
/// calls can be type-checked regardless of declaration order.
struct FuncSig {
    name: String,
    params: Vec<Type>,
    /// How each parameter takes its argument, positionally aligned with
    /// `params`.
    ///
    /// A method's receiver occupies slot 0 of both, and takes
    /// [`OwnershipMode::BorrowRead`]: calling `p.sum()` does not consume `p`.
    param_ownership: Vec<OwnershipMode>,
    return_type: Type,
    name_span: Span,
    is_main: bool,
}

/// Analyzes a parsed program.
///
/// `modules` names every module the program was loaded with and the file each
/// one is, so an `import` that names something else can be reported as
/// unresolved. A single-file program passes an empty slice.
///
/// `build_kind` decides the entrypoint rule: an application must declare a
/// `@Main` and a library must not. That check lives here, above the backend
/// split, which is why the kind is a frontend input rather than a backend flag.
pub fn analyze(
    tree: &SyntaxTree,
    interner: &Interner,
    modules: &[(String, SourceId)],
    build_kind: BuildKind,
) -> Analysis {
    Analyzer::new(tree, interner, modules, build_kind).run()
}

pub(crate) struct Analyzer<'a> {
    /// The file whose item is being analyzed right now.
    ///
    /// One tree spans every file of the program, so this moves as the analyzer
    /// walks: it is what a diagnostic's span is attributed to, and what decides
    /// which file's imports a qualified name resolves against.
    pub(crate) source: SourceId,
    /// Whether this program is an application (needs `@Main`) or a library
    /// (must not have one).
    pub(crate) build_kind: BuildKind,
    /// Every file's imports, keyed by file.
    pub(crate) imports: crate::imports::ImportTable,
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
    /// Each declared enum's per-variant payload defaults, as written, indexed
    /// by [`kira_semantics_model::EnumId`] and then by variant index.
    ///
    /// Kept beside the table for the same reason `struct_defaults` is: a default
    /// is unanalyzed syntax, and the table is a model type that carries none. A
    /// construction site analyzes only the default it needs.
    pub(crate) enum_defaults: Vec<Vec<Option<ExprId>>>,
    /// Every `type Name = Target` alias, keyed by name.
    ///
    /// Registered before anything is resolved and consulted from
    /// `resolve_named_type`, so an alias reaches every type position at once.
    pub(crate) aliases: AliasTable,
    /// Per-class flattening results, keyed by the struct id the class was
    /// declared as.
    ///
    /// A class *is* a struct by the time anything downstream sees it, so this
    /// is the only place that remembers which struct ids came from a class and
    /// what each one inherited. It never leaves analysis.
    pub(crate) classes: HashMap<StructId, crate::classes::ClassInfo>,
    /// The methods each struct and class declares itself, keyed by id.
    ///
    /// Kept beside the struct table because a method is not part of a struct's
    /// shape — the table carries layout, and this carries what was written.
    pub(crate) own_methods: HashMap<StructId, Vec<crate::classes::OwnMethod>>,
    /// Classes dropped before flattening because their parents form a cycle.
    ///
    /// Kept so a class that merely *names* one is not reported a second time
    /// for a parent that exists in the source but not in the table.
    pub(crate) unflattenable_classes: BTreeSet<String>,
    /// Every function type the program mentions, and the struct each became.
    ///
    /// Beside the struct table for the same reason `classes` is: a function
    /// type *is* a struct by the time anything downstream sees it, and this is
    /// the only place that remembers which struct ids came from one. It never
    /// leaves analysis.
    pub(crate) fn_types: crate::closures::FnTypeTable,
    /// The id every synthesized function is offset from: the number of
    /// functions the source declares.
    pub(crate) synth_base: u32,
    /// Synthesized function bodies — lifted closures and dispatchers — indexed
    /// by their id less [`Analyzer::synth_base`].
    pub(crate) synth: Vec<Option<HirFunction>>,
    /// Each closure literal's value, waiting for its type's field list to stop
    /// growing.
    pub(crate) closure_sites: Vec<crate::closures::ClosureSite>,
    /// The engine the function being analyzed runs on, so a closure lifted out
    /// of its body runs on the same one.
    pub(crate) current_execution: kira_semantics_model::Execution,
    pub(crate) program: HirProgram,
    pub(crate) diagnostics: Vec<Diagnostic>,
}

impl<'a> Analyzer<'a> {
    fn new(
        tree: &'a SyntaxTree,
        interner: &'a Interner,
        modules: &[(String, SourceId)],
        build_kind: BuildKind,
    ) -> Self {
        let entries = crate::imports::collect_imports(tree, interner);
        let imports = crate::imports::ImportTable::build(modules, &entries);
        let mut analyzer = Self {
            source: crate::FILE_SOURCE_ID,
            build_kind,
            imports,
            tree,
            interner,
            sigs: Vec::new(),
            sig_index: HashMap::new(),
            struct_defaults: Vec::new(),
            enum_defaults: Vec::new(),
            aliases: AliasTable::new(),
            classes: HashMap::new(),
            own_methods: HashMap::new(),
            unflattenable_classes: BTreeSet::new(),
            fn_types: crate::closures::FnTypeTable::default(),
            synth_base: 0,
            synth: Vec::new(),
            closure_sites: Vec::new(),
            current_execution: kira_semantics_model::Execution::Inherited,
            program: HirProgram::default(),
            diagnostics: Vec::new(),
        };
        analyzer.report_unresolved_imports(&entries);
        analyzer
    }

    fn run(mut self) -> Analysis {
        // Aliases are registered first because any of the three collections
        // below may name one; they resolve lazily on first use, so registering
        // them here does not require the struct or enum table to exist yet.
        self.collect_type_aliases();
        // Enums are declared before structs, so a struct field may name one; a
        // struct is declared before signatures, so a parameter may name either.
        self.collect_enums();
        self.collect_structs();
        // Classes flatten into the same table, and may extend a struct, so they
        // are declared once every struct exists.
        self.collect_classes();
        let callables = self.callables();
        // Every synthesized function sits after every declared one, so the
        // declared count is the offset a reserved id is measured from. Fixed
        // here, before any signature can reserve one.
        self.synth_base = callables.len() as u32;
        self.collect_signatures(&callables);
        // `@Main` is a property of the program, not of any one file, and the
        // "no `@Main`" diagnostic has no span to point at — so it is attributed
        // to the entry file rather than to whichever module happened to declare
        // the last signature.
        self.source = crate::FILE_SOURCE_ID;
        self.check_main();
        // Exports are checked once signatures exist, because every refusal is
        // about a *resolved* parameter or result type, and once classes are
        // flattened, because handle-eligibility is a property of a struct row.
        self.check_exports(&callables);
        // Bodies are analyzed in the same order the signatures were collected,
        // which is what makes a `FuncId` index both.
        for (index, callable) in callables.iter().enumerate() {
            let hir_function = self.analyze_function(FuncId(index as u32), callable);
            self.program.functions.push(hir_function);
        }
        // Lifted closure bodies and dispatchers are appended here, which is the
        // one point at which every function type's literal set is final.
        self.finalize_closures();
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
        for (source, item) in self.tree.items_with_source() {
            match item {
                Item::Function(function) => callables.push(Callable {
                    receiver: None,
                    origin: None,
                    function,
                    source,
                }),
                Item::Struct(declaration) => {
                    let owner = self
                        .program
                        .types
                        .structs()
                        .lookup(self.interner.resolve(declaration.name));
                    for method in &declaration.methods {
                        callables.push(Callable {
                            receiver: owner,
                            origin: None,
                            function: method,
                            source,
                        });
                    }
                }
                Item::Class(declaration) => {
                    self.class_callables(declaration, source, &mut callables)
                }
                Item::Enum(_) | Item::TypeAlias(_) | Item::Import(_) | Item::Unsupported(_) => {}
            }
        }
        callables
    }

    fn collect_signatures(&mut self, callables: &[Callable<'a>]) {
        let mut main_seen = false;
        for callable in callables {
            let function = callable.function;
            // A signature's types are written in the file the function was, so
            // they resolve against that file's imports.
            self.source = callable.source;
            let name = self.callable_name(*callable);
            // A method's receiver is parameter 0, so its signature carries the
            // struct type ahead of what was written.
            let mut params: Vec<Type> = callable.receiver.map(Type::Struct).into_iter().collect();
            params.extend(
                function
                    .params
                    .iter()
                    .map(|param| self.resolve_type_ref(param.ty)),
            );
            // A method's receiver borrows: `p.sum()` reads `p` and leaves it
            // usable. The oracle says the same — an unannotated receiver is
            // `borrow_read` — so a method call never demands `move`.
            let mut param_ownership: Vec<OwnershipMode> = callable
                .receiver
                .map(|_| OwnershipMode::BorrowRead)
                .into_iter()
                .collect();
            for param in &function.params {
                param_ownership.push(self.check_param_ownership(param));
            }
            let return_type = match &function.return_type {
                Some(type_ref) => self.resolve_type_ref(*type_ref),
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
                param_ownership,
                return_type,
                name_span: function.name_span,
                is_main,
            });
        }
    }

    /// The mode a parameter declares, reporting the one mode this port does
    /// not implement.
    ///
    /// `borrow mut` is the single ownership mode that is **observable at run
    /// time**: a callee writing through it must change the caller's binding.
    /// Every other mode reduces to the deep copy the runtime already does, so
    /// it lands as a pure static check. Accepting `borrow mut` without the
    /// by-reference calling convention would not be an incomplete feature —
    /// it would silently compute wrong answers, because the callee would
    /// mutate a copy and the caller would never see the write.
    ///
    /// So it is refused with a typed error until the backends carry it,
    /// following the oracle's own precedent for a reserved-but-unimplemented
    /// mode (`copy` of a non-trivial value is `KSEM116` there for exactly this
    /// reason). `KSEM112` is the free code in the ownership band.
    fn check_param_ownership(&mut self, param: &kira_syntax_model::ast::Param) -> OwnershipMode {
        if param.ownership == OwnershipMode::BorrowMut {
            let span = param.ownership_span.unwrap_or(param.span);
            self.emit(
                span,
                "KSEM112",
                "Kira parsed `borrow mut`, but a mutable borrow is not implemented yet: \
                 the callee would write to a copy the caller never sees. Take the value \
                 with `move` and return the updated one, or use `borrow` to read it.",
            );
        }
        // The mode is returned unchanged even when refused. Rewriting it to
        // something implementable would make the body and the call sites check
        // against a signature nobody wrote — a `borrow mut` body writing to
        // its parameter would collect a spurious "cannot assign" on top of the
        // real problem. The program is already rejected; every other
        // diagnostic it collects should still be about what it said.
        param.ownership
    }

    /// The name a callable is known by.
    ///
    /// A method is qualified with its struct (`Point.sum`), which is what keeps
    /// two structs' methods of the same name apart and keeps a method from
    /// colliding with a free function — `.` cannot appear in an identifier, so
    /// no user name can collide with a qualified one.
    pub(crate) fn callable_name(&self, callable: Callable<'_>) -> String {
        let written = self.interner.resolve(callable.function.name);
        let Some(id) = callable.receiver else {
            return written.to_owned();
        };
        let receiver = self.program.types.type_name(Type::Struct(id));
        // A class carries one copy of every method any ancestor declares. The
        // copy that wins bare lookup takes the plain `Class.method` name a call
        // site spells; a copy an override shadows takes a qualified name, which
        // is what `ClsSquare.scaledArea()` inside `ClsCube` resolves to. `$`
        // cannot appear in an identifier, so neither can collide with a user
        // name.
        match callable.origin {
            Some(origin) if !self.is_most_derived(id, origin, written) => {
                let origin = self.program.types.type_name(Type::Struct(origin));
                format!("{receiver}.{origin}${written}")
            }
            _ => format!("{receiver}.{written}"),
        }
    }

    /// Whether `origin`'s copy of `method` is the one bare lookup on `class`
    /// finds.
    pub(crate) fn is_most_derived(&self, class: StructId, origin: StructId, method: &str) -> bool {
        matches!(
            self.classes
                .get(&class)
                .and_then(|info| info.bare_methods.get(method)),
            Some(crate::classes::Member::One(winner)) if *winner == origin
        )
    }

    /// Checks the entrypoint rule for the kind of thing being built.
    ///
    /// An application needs exactly one `@Main`; a library must have none. Both
    /// halves are decided here rather than in a backend because the answer is
    /// the same for every backend: an entrypoint is a property of the program,
    /// not of the engine that runs it.
    fn check_main(&mut self) {
        // Snapshot the entrypoint's identity before emitting, so the
        // immutable borrow of `self.sigs` does not overlap `self.emit`.
        let main = self
            .sigs
            .iter()
            .find(|sig| sig.is_main)
            .map(|sig| (sig.name.clone(), sig.params.is_empty(), sig.name_span));
        match (self.build_kind, main) {
            (BuildKind::Application, None) => {
                self.emit(
                    Span::new(0, 0),
                    "KSEM011",
                    "program has no `@Main` function to run",
                );
            }
            (BuildKind::Application, Some((name, no_params, name_span))) => {
                if !no_params {
                    self.emit(name_span, "KSEM012", "`@Main` must take no parameters");
                }
                self.program.main = self.sig_index.get(&name).copied();
            }
            // A library has no entrypoint by definition, so its absence is not
            // an error and `program.main` stays `None`.
            (BuildKind::Library, None) => {}
            (BuildKind::Library, Some((_, _, name_span))) => {
                self.emit(
                    name_span,
                    "KSEM158",
                    "a library package cannot declare `@Main`: a library is \
                     entered by its consumer, not run",
                );
            }
        }
    }

    fn analyze_function(&mut self, id: FuncId, callable: &Callable<'a>) -> HirFunction {
        let function = callable.function;
        // The body resolves qualified names against the imports of the file it
        // was written in — not the entry file's, and not the union of all of
        // them. That is what "file-scoped" means.
        self.source = callable.source;
        let sig_return = self.sigs[id.0 as usize].return_type;
        self.current_execution = function.execution;
        let mut ctx = FnCtx::new(sig_return);
        // A method's receiver is local 0, named `self`. It is immutable: a
        // method receives a copy like any other by-value parameter, so writing
        // to it would change nothing the caller could see, and letting it look
        // like it might would be worse than refusing.
        if let Some(owner) = callable.receiver {
            ctx.declare_param(
                "self",
                Type::Struct(owner),
                false,
                OwnershipMode::BorrowRead,
            );
            ctx.receiver = Some(owner);
        }
        // Parameters become the next locals, each carrying the mode its
        // declaration asked for. Reading the mode off the signature rather
        // than off the syntax again keeps the `borrow mut` refusal from being
        // reported a second time here.
        let param_modes = self.sigs[id.0 as usize].param_ownership.clone();
        let receiver_slots = usize::from(callable.receiver.is_some());
        for (index, param) in function.params.iter().enumerate() {
            let ty = self.resolve_type_ref(param.ty);
            let name = self.interner.resolve(param.name).to_owned();
            let mode = param_modes
                .get(index + receiver_slots)
                .copied()
                .unwrap_or(OwnershipMode::Owned);
            // A `borrow mut` parameter is the only kind a body may write
            // through; every other parameter is an immutable binding.
            let mutable = mode == OwnershipMode::BorrowMut;
            ctx.declare_param(&name, ty, mutable, mode);
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
        if self.as_function_type(base_ty).is_some() {
            return false;
        }
        matches!(base_ty, Type::Struct(id)
            if self
                .program.types.structs().get(id)
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
        // A function type is a struct only because that is how closures are
        // desugared. The oracle pins no member access on a function value, so
        // letting the ordinary field path run here would publish the
        // representation — `f.tag` — as invented surface.
        if self.as_function_type(base_ty).is_some() {
            self.emit(
                span,
                "KSEM136",
                format!(
                    "`{}` is a function; a function has no members, only a call",
                    self.type_name(base_ty)
                ),
            );
            return None;
        }
        let resolved = self
            .program
            .types
            .structs()
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

    /// The ownership mode each parameter of `id` declares, receiver included.
    pub(crate) fn param_ownership(&self, id: FuncId) -> Vec<OwnershipMode> {
        self.sigs[id.0 as usize].param_ownership.clone()
    }

    /// The spelling of `ty` for a diagnostic.
    ///
    /// Owned rather than borrowed on purpose: a struct's name lives in
    /// `self.program` and an array's is built on demand, and holding a borrow
    /// across an `emit` — which needs `&mut self` — would not compile.
    pub(crate) fn type_name(&self, ty: Type) -> String {
        self.program.types.type_name(ty)
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
