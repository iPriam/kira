//! The semantic analyzer: turns a syntax tree into a typed [`HirProgram`].
//!
//! Analysis is a total function: it always produces a program plus a list of
//! diagnostics and never bails on the first error. Unresolved names and type
//! mismatches become [`HirExpr::Error`] nodes (type `Error`), which the type
//! lattice treats as compatible everywhere so one mistake does not cascade.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use kira_core::Interner;
use kira_diagnostics::{Diagnostic, Label, Severity};
use kira_semantics_model::hir::{FuncId, HirExprId, HirFunction, HirProgram};
use kira_semantics_model::{EnumId, OwnershipMode, StructId, Type};
use kira_source::{FileSpan, SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{ExprId, Function, Item};

mod scope;
mod signatures;

pub(crate) use scope::FnCtx;
pub(crate) use signatures::FuncSig;

use crate::aliases::AliasTable;
use crate::build_kind::BuildKind;

/// The result of analyzing one program.
#[derive(Debug, Clone)]
pub struct Analysis {
    /// The typed program (always produced, possibly containing error nodes).
    pub program: HirProgram,
    /// Diagnostics discovered during analysis.
    pub diagnostics: Vec<Diagnostic>,
    /// Every reference the analyzer resolved, linked to its definition.
    pub definitions: Vec<crate::DefinitionLink>,
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

/// One declared default initializer — a struct field's or a function
/// parameter's — bound to the file where it was declared.
///
/// Both resolve the same way: once, in the declaring file's scope, reusing the
/// resulting HIR at every site that omits the field or argument.
#[derive(Clone, Copy)]
pub(crate) struct FieldDefault {
    /// The default expression as written.
    pub(crate) syntax: ExprId,
    /// The declaring file whose imports and package scope resolve its names.
    pub(crate) source: SourceId,
    /// The name-resolved, typed expression shared by every use site.
    pub(crate) resolved: Option<HirExprId>,
}

impl FieldDefault {
    /// Records an unresolved default in its declaring file.
    pub(crate) fn new(syntax: ExprId, source: SourceId) -> Self {
        Self {
            syntax,
            source,
            resolved: None,
        }
    }
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
    pub(crate) sig_index: HashMap<String, FuncId>,
    /// Whether each callable, by [`FuncId`], is a method that mutates its
    /// receiver. Computed to a fixpoint after signatures and before bodies (see
    /// [`crate::mutation`]); empty until then.
    pub(crate) mutating_methods: Vec<bool>,
    /// Every `@FFI.Extern` callable the program accepted, keyed by its Kira
    /// name, mapping to its row in [`HirProgram::foreign`].
    ///
    /// A refused extern is never inserted, so a call name found here is one the
    /// signature and annotation checks approved. A foreign name may not collide
    /// with a user function's, which is what keeps a call resolving to exactly
    /// one [`kira_semantics_model::hir::Callee`].
    pub(crate) foreign_index: HashMap<String, kira_semantics_model::hir::ForeignId>,
    /// Whether the type being resolved sits in an `@FFI.Extern` signature.
    ///
    /// `CString` is legal only as a foreign parameter, so its seam-only refusal
    /// (`resolve_named_type`) is suppressed while this is set; the foreign pass
    /// then decides, per position, whether the `CString` is a legal parameter or
    /// an illegal result. Every other position resolves with this `false`, so a
    /// written `CString` there is refused where it is resolved.
    pub(crate) in_foreign_signature: bool,
    /// Each declared struct's field defaults, indexed by
    /// [`kira_semantics_model::StructId`] and then by field index.
    ///
    /// Kept beside the table rather than in it because the type table is a model
    /// with no syntax or HIR. Each row remembers the declaring file and is
    /// resolved once after signatures exist; every construction site then reuses
    /// the same name-resolved expression.
    pub(crate) struct_defaults: Vec<Vec<Option<FieldDefault>>>,
    /// Defaults currently being resolved, guarding recursive default expansion.
    pub(crate) resolving_struct_defaults: BTreeSet<(u32, u32)>,
    /// Each function's parameter defaults, indexed by [`FuncId`] and then by
    /// parameter slot, receiver included (a receiver slot is always `None`).
    ///
    /// Kept beside the signature table for the same reason `struct_defaults` is
    /// kept beside the type table: a default is unanalyzed syntax and the model
    /// carries none. Each row remembers its declaring file and is resolved once
    /// after signatures exist; every call that omits the argument reuses the
    /// same name-resolved expression.
    pub(crate) param_defaults: Vec<Vec<Option<FieldDefault>>>,
    /// Parameter defaults currently being resolved, guarding a default that
    /// fills itself through the call graph (`f(x = g())`, `g(y = f())`).
    pub(crate) resolving_param_defaults: BTreeSet<(u32, u32)>,
    /// Each declared enum's per-variant payload defaults, as written, indexed
    /// by [`kira_semantics_model::EnumId`] and then by variant index.
    ///
    /// Kept beside the table for the same reason `struct_defaults` is: a default
    /// is unanalyzed syntax, and the table is a model type that carries none. A
    /// construction site analyzes only the default it needs.
    pub(crate) enum_defaults: Vec<Vec<Option<ExprId>>>,
    /// Every generic enum declaration, keyed by name.
    ///
    /// A generic declaration names no type: it waits here until a written
    /// instantiation substitutes its arguments and declares the result in the
    /// ordinary enum table. See [`crate::generics`].
    pub(crate) generic_enums: crate::generics::GenericEnumTable<'a>,
    /// The type-parameter substitution in force right now, empty outside a
    /// generic enum's body.
    pub(crate) type_bindings: crate::generics::TypeBindings,
    /// How many generic instantiations are open, which is what bounds a
    /// template that grows its own argument.
    pub(crate) instantiation_depth: u32,
    /// Where to blame an unsupported enum payload, when a generic
    /// instantiation is what produced it.
    ///
    /// A payload type written inside a template resolves to whatever the
    /// *arguments* say, so the mistake belongs to the instantiation site, not
    /// to the template's own `Ok(Value)`. `None` outside an instantiation.
    pub(crate) payload_blame: Option<(SourceId, Span)>,
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
    /// Per-construct-backed-declaration results, keyed by the struct id it was
    /// compiled to. The only record that a struct id came from a construct, and
    /// which of its members are computed bridges read as properties.
    pub(crate) constructs: HashMap<StructId, crate::constructs::ConstructInfo>,
    /// Construct families keyed by their source name.
    pub(crate) construct_families: BTreeMap<String, crate::constructs::ConstructFamilyInfo<'a>>,
    /// Reverse lookup from synthesized family enum to source family name.
    pub(crate) construct_family_names: HashMap<EnumId, String>,
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
    /// Which declared struct ids came from a `@FFI.*` type annotation, and
    /// which form. Only `@FFI.Struct`/`Array`/`Callback` mint a struct id;
    /// `@FFI.Alias`/`Pointer` become aliases and never appear here. This is
    /// where a C-layout struct's zero-fill construction and an array's or
    /// callback's typed "not yet executable" refusals read their answer.
    pub(crate) ffi_structs: HashMap<StructId, crate::ffi_types::FfiStructKind>,
    /// The file each declared struct was written in.
    ///
    /// Kept because a bare name is resolved against one program-wide table
    /// while a *declaration* belongs to one package, and telling two same-named
    /// declarations apart needs to know which package each came from.
    pub(crate) struct_sources: HashMap<StructId, SourceId>,
    /// The C extent of each `@FFI.Array` type, which its Kira type does not
    /// carry: a Kira array's length is its own, while the C declaration reserves
    /// exactly this many elements.
    pub(crate) ffi_array_counts: HashMap<StructId, u32>,
    /// The C signature each `@FFI.Callback` type declares, resolved once at its
    /// declaration. A Kira function named where one of these types is expected
    /// is checked against the signature recorded here.
    pub(crate) ffi_callback_signatures: HashMap<StructId, kira_runtime_abi::ForeignSignature>,
    /// Keeps each C-layout aggregate in the program table exactly once.
    pub(crate) foreign_aggregates: crate::foreign_aggregate::ForeignAggregateBuilder,
    pub(crate) program: HirProgram,
    pub(crate) diagnostics: Vec<Diagnostic>,
    /// Reference→definition links, recorded as names resolve.
    pub(crate) definitions: Vec<crate::DefinitionLink>,
    /// Declaration name spans, indexed from the tree before resolution runs.
    pub(crate) decl_spans: crate::definitions::DeclSpans,
}

impl Analyzer<'_> {
    /// The struct `name` denotes *here*, or `None` when no declaration of that
    /// name is visible from the file being analyzed.
    ///
    /// The table is program-wide because one program is compiled at once, but a
    /// name is not: a declaration is nameable bare only from its own package or
    /// from a file that imports that package. See
    /// [`crate::imports::ImportTable::sees`] for the rule, and why it does not
    /// compose through a dependency's own imports.
    pub(crate) fn visible_struct(&self, name: &str) -> Option<StructId> {
        let id = self.program.types.structs().lookup(name)?;
        self.struct_is_visible(id).then_some(id)
    }

    /// Whether the declaration behind `id` is nameable from the current file.
    pub(crate) fn struct_is_visible(&self, id: StructId) -> bool {
        match self.struct_sources.get(&id) {
            Some(declared) => self.imports.sees(self.source, *declared),
            // A struct with no recorded declaration is synthesized (a closure's
            // environment, a construct family's dispatcher): the compiler made
            // it for this program, so it belongs to whoever is asking.
            None => true,
        }
    }
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
            mutating_methods: Vec::new(),
            foreign_index: HashMap::new(),
            in_foreign_signature: false,
            struct_defaults: Vec::new(),
            resolving_struct_defaults: BTreeSet::new(),
            param_defaults: Vec::new(),
            resolving_param_defaults: BTreeSet::new(),
            enum_defaults: Vec::new(),
            generic_enums: crate::generics::GenericEnumTable::new(),
            type_bindings: crate::generics::TypeBindings::new(),
            instantiation_depth: 0,
            payload_blame: None,
            aliases: AliasTable::new(),
            classes: HashMap::new(),
            constructs: HashMap::new(),
            construct_families: BTreeMap::new(),
            construct_family_names: HashMap::new(),
            own_methods: HashMap::new(),
            unflattenable_classes: BTreeSet::new(),
            fn_types: crate::closures::FnTypeTable::default(),
            synth_base: 0,
            synth: Vec::new(),
            closure_sites: Vec::new(),
            current_execution: kira_semantics_model::Execution::Inherited,
            ffi_structs: HashMap::new(),
            struct_sources: HashMap::new(),
            ffi_array_counts: HashMap::new(),
            ffi_callback_signatures: HashMap::new(),
            foreign_aggregates: crate::foreign_aggregate::ForeignAggregateBuilder::default(),
            program: HirProgram::default(),
            diagnostics: Vec::new(),
            definitions: Vec::new(),
            decl_spans: crate::definitions::DeclSpans::collect(tree, interner),
        };
        analyzer.report_unresolved_imports(&entries);
        analyzer.link_resolved_imports(&entries);
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
        // A family type must exist before ordinary structs resolve fields that
        // name it; concrete variants are filled after backed structs exist.
        self.collect_construct_family_headers();
        // `extend Family { … }` modifiers join the family's method surface once
        // the family exists and before its method signatures are resolved, so a
        // modifier's parameter and result types are resolved with the rest.
        self.collect_extend_blocks();
        self.collect_structs();
        // Classes flatten into the same table, and may extend a struct, so they
        // are declared once every struct exists.
        self.collect_classes();
        // Construct-backed declarations become struct-shaped types too, and a
        // param or member may name any struct, enum, or class, so they are
        // declared once every one of those exists and before signatures — a
        // backed declaration's methods take signature slots.
        self.collect_constructs();
        let callables = self.callables();
        // Every synthesized function sits after every declared one, so the
        // declared count is the offset a reserved id is measured from. Fixed
        // here, before any signature can reserve one.
        self.synth_base = callables.len() as u32;
        self.collect_signatures(&callables);
        // Each `extend` modifier lowers to one synthesized function; its id is
        // reserved here, once `synth_base` is fixed, so an uncalled modifier is
        // still checked and lowered. The bodies are filled after ordinary ones.
        self.reserve_extend_bodies();
        // Which methods mutate their receiver is decided once here, before any
        // body is analyzed: a body analyzes `self` as mutable exactly when its
        // method is marked mutating, and a call site writes the receiver back
        // exactly when its callee is. The fixpoint reads the signatures the step
        // above built, so it runs after them.
        self.collect_mutating_methods(&callables);
        self.check_construct_method_signatures();
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
        // Foreign callables are collected once signatures exist — a foreign name
        // may not collide with a user function's, and the collision check reads
        // the signature index — and before any body, so a call in a body
        // resolves to `Callee::Foreign`.
        self.collect_foreign();
        // A field default belongs to its declaration. Resolve every one now, with
        // signatures and foreign callables available but before a construction
        // site can supply some unrelated file scope, and reuse that HIR at every
        // site that omits the field.
        self.resolve_struct_defaults();
        // A parameter default belongs to its declaration too, and follows the
        // same rule: resolve every one now, in its declaring file, and reuse the
        // HIR at every call that omits the argument.
        self.resolve_param_defaults();
        // A construct-family method carries no signature row, so its parameter
        // defaults resolve in their own pass — same rule, same moment.
        self.resolve_construct_method_defaults();
        // Bodies are analyzed in the same order the signatures were collected,
        // which is what makes a `FuncId` index both.
        for (index, callable) in callables.iter().enumerate() {
            let hir_function = self.analyze_function(FuncId(index as u32), callable);
            self.program.functions.push(hir_function);
        }
        // Dynamic construct dispatchers and closure dispatchers share the same
        // synthesized-function id space and are all filled before it is appended.
        self.build_construct_dispatchers();
        // Modifier bodies share the synthesized-function id space with the
        // dispatchers and are filled before it is appended by the closures pass.
        self.build_extend_methods();
        self.finalize_closures();
        Analysis {
            program: self.program,
            diagnostics: self.diagnostics,
            definitions: self.definitions,
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
                // A bodyless `@FFI.Extern` function is never an ordinary
                // callable: it becomes a row in `HirProgram::foreign`, not a
                // `HirFunction`, so it is skipped here and handled by
                // `collect_foreign`.
                Item::Function(function) if function.foreign.is_some() => {}
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
                Item::Construct(declaration) => {
                    self.construct_callables(declaration, source, &mut callables)
                }
                // An `extend` block's modifiers are not ordinary callables:
                // each lowers to a synthesized function whose receiver is the
                // family value, built after signatures exist. See
                // `constructs::extend`.
                Item::Enum(_)
                | Item::TypeAlias(_)
                | Item::Import(_)
                | Item::Extend(_)
                | Item::Unsupported(_) => {}
            }
        }
        callables
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
        // A method's receiver is local 0, named `self`. A non-mutating method
        // receives it as an immutable copy — writing to it would change nothing
        // the caller could see. A mutating method receives it as a mutable,
        // owned value that the call site writes back afterwards, so `self.field
        // = x` is a real write to the caller's storage rather than a lost one.
        if let Some(owner) = callable.receiver {
            let mutates = self.mutates_self(id);
            let (mutable, mode) = if mutates {
                (true, OwnershipMode::Owned)
            } else {
                (false, OwnershipMode::BorrowRead)
            };
            ctx.declare_param("self", Type::Struct(owner), mutable, mode);
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
            let local = ctx.declare_param(&name, ty, mutable, mode);
            ctx.note_binding_span(local, param.name_span);
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
            mutates_self: self.mutates_self(id),
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
