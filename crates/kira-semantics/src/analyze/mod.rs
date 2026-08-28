//! The semantic analyzer: turns a syntax tree into a typed [`HirProgram`].
//!
//! Analysis is a total function: it always produces a program plus a list of
//! diagnostics and never bails on the first error. Unresolved names and type
//! mismatches become [`HirExpr::Error`] nodes (type `Error`), which the type
//! lattice treats as compatible everywhere so one mistake does not cascade.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use kira_core::Names;
use kira_diagnostics::{Code, Diagnostic, Label, Severity};
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
use crate::build_machine::BuildMachine;

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
#[derive(Clone)]
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
    /// Parameters whose declared class is replaced by a subclass, by index into
    /// the *written* parameter list.
    ///
    /// This is subtyping without a vtable, and it is the same trick `origin`
    /// plays for inheritance: a function taking `Animal` is registered again for
    /// each class that inherits `Animal`, with that parameter typed as the
    /// subclass. `a.speak()` inside the copy is therefore statically the
    /// subclass's `speak`, so an override wins with nothing to dispatch at run
    /// time. Empty for the function as written.
    pub(crate) specialize: Vec<(usize, StructId)>,
    /// The construct-backed declaration this is an `init(…)` of.
    ///
    /// An initializer produces a value rather than running on one, so it carries
    /// no [`receiver`](Callable::receiver): it is a free function whose result is
    /// the declaration. Set here so the signature pass can give it that result
    /// and a name of the declaration's own.
    pub(crate) initializes: Option<StructId>,
    /// The declaration as written.
    pub(crate) function: &'a Function,
    /// The file the declaration was written in.
    ///
    /// Carried so a diagnostic about this function points into the right file,
    /// and so its body resolves qualified names against *that* file's imports —
    /// which is the whole of file scoping.
    pub(crate) source: SourceId,
}

/// The parameter of one `init(…)` that its construction's trailing children
/// fill.
#[derive(Clone, Copy)]
pub(crate) struct InitContent {
    /// Which parameter it is. Always the last one: the children are written
    /// after every parenthesized argument, so they fill the slot that follows
    /// them.
    pub(crate) slot: usize,
    /// Whether it holds an ordered list rather than exactly one child.
    pub(crate) list: bool,
    /// The type each child must satisfy.
    pub(crate) element: Type,
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
///
/// `machine` is which machine the program is being built for. A declaration can
/// be unavailable on one — an `@FFI.Syscall` naming a Linux system call is not
/// reachable from a macOS binary and has no lowering on a 32-bit processor — and
/// refusing it here, by name, is what keeps a wrong number out of an emitted
/// object.
pub fn analyze(
    tree: &SyntaxTree,
    interner: &Names,
    modules: &[(String, SourceId)],
    build_kind: BuildKind,
    machine: &BuildMachine,
) -> Analysis {
    Analyzer::new(tree, interner, modules, build_kind, machine).run()
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
    /// The machine this program is being built for.
    ///
    /// Owned rather than borrowed because it outlives no analysis and is read
    /// once per foreign declaration: a clone of two short strings, against a
    /// lifetime parameter on every path that reaches a declaration check.
    pub(crate) machine: BuildMachine,
    /// Every file's imports, keyed by file.
    pub(crate) imports: crate::imports::ImportTable,
    pub(crate) tree: &'a SyntaxTree,
    pub(crate) interner: &'a Names,
    pub(crate) sigs: Vec<FuncSig>,
    /// Every declaration answering to one written name, in declaration order.
    ///
    /// A name carries more than one entry when it is **overloaded**: several
    /// declarations share it and are told apart by what they take. Resolution
    /// picks among them at each call; the vector is what makes that possible,
    /// and a name declared once has a vector of one.
    pub(crate) sig_index: HashMap<String, Vec<FuncId>>,
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
    /// Instantiations whose bounded parameters still owe their discharge, in
    /// mint order. Answered by
    /// [`Analyzer::check_pending_generic_bounds`](crate::generics) once the
    /// conformance table and drop facts are final; see
    /// [`crate::generics::PendingBoundCheck`].
    pub(crate) pending_bounds: Vec<crate::generics::PendingBoundCheck>,
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
    /// What each `@FFI.Pointer` alias points at, by name — alias name to
    /// written target name.
    ///
    /// Names only, recorded when the alias is collected and never resolved: a
    /// generated binding points at plenty of C types nobody declares, and those
    /// are opaque handles rather than mistakes. A parameter written as one of
    /// these is a pointer word at the seam and *also* accepts the struct that
    /// name refers to, when there is one, which is how `sapp_run(move desc)`
    /// hands a descriptor over by address.
    pub(crate) pointer_targets: HashMap<String, String>,
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
    /// Every declared trait, keyed by name.
    ///
    /// Collected from syntax before any type table exists, because one type
    /// namespace means every other declaration has to be able to lose a name
    /// collision to a trait.
    pub(crate) traits: crate::traits::TraitTable<'a>,
    /// Every conformance the program declares, in source order.
    ///
    /// Beside the type table rather than in it: conformance is resolved away
    /// before the HIR exists, so nothing downstream carries it.
    pub(crate) conformances: Vec<crate::traits::Conformance>,
    /// Member and element reads of a value that runs a user `Drop`, waiting for
    /// the body being analyzed to finish.
    ///
    /// A read is refused only if no enclosing expression claimed it as a
    /// borrowed one, and the enclosing expression is built after the read — so
    /// the answer is not available until the body is whole. See
    /// [`crate::traits::drop`].
    pub(crate) drop_extractions: Vec<crate::traits::drop::DropExtraction>,
    /// Every enum variant payload the program resolved, as
    /// `(type, declaring file, span to blame)`.
    ///
    /// Kept because whether a payload runs a user `Drop` is a question about a
    /// conformance, which is collected after every payload is resolved — and
    /// the span an instantiation should be blamed at is worked out by the
    /// payload pass and by nothing else.
    pub(crate) enum_payload_sites: Vec<(Type, SourceId, Span)>,
    /// Reverse lookup from synthesized family enum to source family name.
    pub(crate) construct_family_names: HashMap<EnumId, String>,
    /// Every trait existential reserved so far, keyed by trait name.
    ///
    /// See [`crate::traits::existential`].
    pub(crate) trait_existentials: BTreeMap<String, crate::traits::existential::TraitExistential>,
    /// Reverse lookup from synthesized existential enum to trait name.
    pub(crate) existential_traits: HashMap<EnumId, String>,
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
    /// The **content parameter** of each `init(…)` that declared one, by
    /// [`FuncId`] index.
    ///
    /// An init parameter written `some X` / `[some X]` takes the construction's
    /// trailing children rather than a written argument, the way a declaration's
    /// child slot does. This is what makes `NavigationLink(value: v) { Text(…) }`
    /// reach an init instead of only the parenthesized header.
    pub(crate) init_content: HashMap<u32, InitContent>,
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
    /// where C-layout zero-fill construction and foreign type validation read
    /// their answers.
    pub(crate) ffi_structs: HashMap<StructId, crate::ffi_types::FfiStructKind>,
    /// The file each declared struct was written in.
    ///
    /// Kept because a bare name is resolved against one program-wide table
    /// while a *declaration* belongs to one package, and telling two same-named
    /// declarations apart needs to know which package each came from.
    pub(crate) struct_sources: HashMap<StructId, SourceId>,
    /// The file each declared enum was written in, for the same reason
    /// [`Self::struct_sources`] is kept: owner-keyed tables answer "whose is
    /// this?" from the declaring file's package.
    pub(crate) enum_sources: HashMap<EnumId, SourceId>,
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
    /// Every module-scope constant, in evaluation order, in lockstep with
    /// [`kira_semantics_model::hir::HirProgram::constants`].
    pub(crate) constants: Vec<crate::constants::ConstantEntry>,
    /// Each constant's slot by name. Names are unique — a clash was refused —
    /// so one index answers a read.
    pub(crate) constant_index: HashMap<String, u32>,
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
    /// Two packages may each declare a `Text`, so the table holds both and this
    /// picks: the file's own package first, then the packages it imports. A
    /// declaration reachable through neither is not nameable here — see
    /// [`crate::imports::ImportTable::sees`] for why that does not compose
    /// through a dependency's own imports.
    ///
    /// Own package first is not a tie-break so much as the rule: a file means
    /// its own package's declaration, and an import cannot take a name away
    /// from the package that wrote it.
    pub(crate) fn visible_struct(&self, name: &str) -> Option<StructId> {
        // This file's own package first: an import cannot take a name away from
        // the package that wrote it.
        let home = self.imports.package_of(self.source);
        self.program
            .types
            .structs()
            .lookup_owned(home, name)
            .or_else(|| self.struct_beyond_own_package(name))
    }

    /// The struct a module-qualified name denotes here.
    ///
    /// The qualifier answers the question the bare rule answers with "mine": a
    /// file that writes `KiraUIFoundation.Color` inside a package that declares
    /// its own `Color` means the other one, so the package owning the qualified
    /// *module* gets first refusal instead of this file's own.
    ///
    /// A package that declares nothing by that name falls through to the same
    /// tail a bare name takes — this file's imports, and nothing further. That
    /// is what lets `KiraUIFoundation.Color` mean the `Color` KiraGraphics
    /// declares (which is what KiraUIFoundation's own API speaks) without
    /// letting visibility compose: the name still has to be one this file can
    /// see on its own.
    pub(crate) fn visible_struct_qualified(
        &self,
        name: &crate::types::QualifiedName,
    ) -> Option<StructId> {
        let Some(module) = name.qualifier else {
            return self.visible_struct(&name.text);
        };
        let owner = self.imports.package_of(module);
        self.program
            .types
            .structs()
            .lookup_owned(owner, &name.text)
            .or_else(|| self.struct_beyond_own_package(&name.text))
    }

    /// Resolution that does not depend on which package the file itself belongs
    /// to: the declarations no package owns, then what this file's imports
    /// provide.
    fn struct_beyond_own_package(&self, name: &str) -> Option<StructId> {
        let structs = self.program.types.structs();
        // The declarations no package owns — a bundled library like
        // `Foundation`, another module of the program, or a struct the compiler
        // synthesized — each on its own terms: a synthesized one belongs to
        // whoever is asking, and a written one needs this file to have imported
        // its module.
        if self.imports.package_of(self.source).is_some()
            && let Some(id) = structs.lookup(name)
            && match self.struct_sources.get(&id) {
                Some(declared) => self.imports.sees(self.source, *declared),
                None => true,
            }
        {
            return Some(id);
        }
        // And finally what the packages this file imports declare.
        self.imports
            .imported_packages(self.source)
            .into_iter()
            .find_map(|package| structs.lookup_owned(Some(&package), name))
    }

    /// The enum `name` denotes *here*, or `None` when no declaration of that
    /// name is visible from the file being analyzed.
    ///
    /// The same rule [`Self::visible_struct`] applies, because the enum table
    /// is owner-keyed for the same reason the struct table is: two packages may
    /// each declare a `Color`, and a bare name means this file's own package's
    /// first.
    pub(crate) fn visible_enum(&self, name: &str) -> Option<EnumId> {
        let home = self.imports.package_of(self.source);
        self.program
            .types
            .enums()
            .lookup_owned(home, name)
            .or_else(|| self.enum_beyond_own_package(name))
    }

    /// The enum a module-qualified name denotes here — `Lib.Color` means the
    /// one the qualified module declares, even from a package that declares its
    /// own `Color`.
    pub(crate) fn visible_enum_qualified(
        &self,
        name: &crate::types::QualifiedName,
    ) -> Option<EnumId> {
        let Some(module) = name.qualifier else {
            return self.visible_enum(&name.text);
        };
        let owner = self.imports.package_of(module);
        self.program
            .types
            .enums()
            .lookup_owned(owner, &name.text)
            .or_else(|| self.enum_beyond_own_package(&name.text))
    }

    /// The enum resolution that does not depend on which package this file
    /// belongs to: rows no package owns (a bundled library's declarations and
    /// the instantiations a generic template minted), then what this file's
    /// imports provide.
    fn enum_beyond_own_package(&self, name: &str) -> Option<EnumId> {
        let enums = self.program.types.enums();
        if self.imports.package_of(self.source).is_some()
            && let Some(id) = enums.lookup(name)
            && match self.enum_sources.get(&id) {
                Some(declared) => self.imports.sees(self.source, *declared),
                None => true,
            }
        {
            return Some(id);
        }
        self.imports
            .imported_packages(self.source)
            .into_iter()
            .find_map(|package| enums.lookup_owned(Some(&package), name))
    }
}

mod callable;
mod field;
mod function;
mod run;
