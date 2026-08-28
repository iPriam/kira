//! Generic enum declarations and the monomorphization that gives them meaning.
//!
//! # A generic enum declares no type
//!
//! `enum Result<Value, Failure> { Ok(Value) Error(Failure) }` names nothing on
//! its own — it is a *template*. What names a type is a written instantiation:
//! `Result<Int, AppError>` substitutes the arguments into the template's
//! variants and declares the result in the ordinary enum table under the
//! mangled name `Result<Int, AppError>`. Two writings of the same instantiation
//! find the same row, because the mangled name is the memo key.
//!
//! # A constructor spells no type arguments
//!
//! `Result.Ok(1)` is the template's name in front of a variant, and the
//! language has no `Result<Int, Bool>.Ok(1)` to write instead — so the
//! *position* supplies the instantiation, exactly as it does for `.Ok(1)`. The
//! name in front only has to agree with what the position already asked for,
//! which is what [`Analyzer::generic_instantiation_expected`] checks against
//! the template recorded for each minted row. Written where nothing asks for an
//! instantiation of that template, it is `KSEM254` — a mistake with its own
//! fix, and never a guess at which instantiation was meant.
//!
//! # Why this costs no backend anything
//!
//! By the time anything below semantics looks, a generic enum has become a
//! plain [`kira_semantics_model::EnumDef`]: a name and a list of variants with
//! resolved payload types. Every backend reads a variant's tag and payload
//! *by id*, never by name — `kira-llvm-backend`'s `enum_payload_type` indexes
//! the same table the VM does, on the host and Web targets alike — so no
//! opcode, no IR node, no runtime tag, and no wire format learns that
//! generics exist. This module is the whole feature.
//!
//! # One model for every generic declaration
//!
//! Enums, structs, classes, traits, and free functions are all templates. A
//! concrete use substitutes its arguments into the ordinary semantic tables;
//! nothing below semantics needs a generic-specific representation.
//!
//! # Bounds
//!
//! A parameter may carry trait bounds (`enum Boxed<Value: Scored>`), written on
//! the declaration and discharged at every instantiation: substituting a type
//! that does not keep the promise is refused, naming the trait and the type
//! (`KSEM315`). The answer cannot be computed where the row is minted — that
//! can be long before the conformance table exists — so the check is queued
//! under the mangled name and answered once the tables it reads are final.
//! A compiler-known marker (`Copyable`, `Send`, `Sync`, `Drop`) may name a
//! bound: unlike a type position, a bound classifies nothing — it constrains
//! which arguments are admitted, and each of those facts is answerable for any
//! argument.
//!
//! # Termination
//!
//! Instantiation is recursive: a template's payload may itself be a generic
//! enum. Memoizing on the mangled name stops direct recursion, but
//! `enum Grow<Value> { More(Grow<[Value]>) }` mints a fresh name every time, so a depth
//! cap is what actually terminates it. Hitting the cap is `KSEM175` — a typed
//! refusal, never a stack overflow.

use std::collections::{HashMap, HashSet};

use kira_semantics_model::{EnumId, FieldDef, Instantiation, StructDef, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{
    ClassDecl, EnumDecl, Function, Item, StructDecl, TraitRef, TypeRefId,
};

use crate::analyze::Analyzer;
use crate::analyze::{Callable, FieldDefault};
use crate::classes::{ClassInfo, OwnMethod};
use crate::traits::is_builtin_trait;
use crate::types::NameContext;

mod aggregate_inference;
mod aggregates;
mod bounds;
mod enums;
mod functions;

/// How deep a chain of generic instantiations may go before it is refused.
///
/// Nothing legitimate nests this far — the corpus never nests at all — so the
/// cap only ever fires on a template that grows its own argument.
const MAX_INSTANTIATION_DEPTH: u32 = 16;

/// One instantiation whose type arguments still owe their parameter bounds.
///
/// Recorded when the row is minted and answered once the conformance table and
/// the drop facts are final, because an instantiation is minted wherever a type
/// resolves — including declaration passes that run before either table
/// exists. `key` is the mangled name, so a memo hit never queues twice.
pub(crate) struct PendingBoundCheck {
    /// The memo key the entry was recorded under.
    key: String,
    /// The template whose parameters carry the bounds.
    template: String,
    /// The substituted arguments, in parameter order.
    args: Vec<Type>,
    /// The file the instantiation was written in.
    source: SourceId,
    /// The file the generic declaration was written in. Bound type arguments
    /// resolve against this file's imports, not the use site.
    declaration_source: SourceId,
    /// The span a refusal points at: the instantiation site, which is where
    /// the fix goes.
    span: Span,
    /// The source-level declaration kind, used only to keep diagnostics
    /// specific without making the bound checker know about each template's
    /// storage representation.
    kind: &'static str,
}

/// One registered `enum Name<Params>` declaration, waiting for a use site.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GenericEnum<'a> {
    /// The declaration as written; instantiation reads its variants from here.
    pub(crate) decl: &'a EnumDecl,
    /// The file it was written in, so its payload types resolve against *that*
    /// file's imports rather than the use site's.
    pub(crate) source: SourceId,
}

/// One frame of type-parameter bindings: the substitution in force while a
/// template's body is being resolved.
///
/// A frame is *replaced*, never stacked, when a nested instantiation begins:
/// an inner template sees only its own parameters, so an outer `Value` can
/// never leak into a body that never declared one.
pub(crate) type TypeBindings = Vec<(String, Type)>;

/// A generic aggregate declaration waiting for a use site to provide its
/// arguments. Structs and classes share one runtime representation, so their
/// templates share the same table too; a class's inheritance metadata is
/// filled when its concrete row is minted.
#[derive(Debug, Clone, Copy)]
pub(crate) enum GenericAggregate<'a> {
    /// A `struct Name<T> { ... }` template.
    Struct {
        /// The declaration as written.
        decl: &'a StructDecl,
        /// The file whose imports resolve its fields and methods.
        source: SourceId,
    },
    /// A `class Name<T> { ... }` template.
    Class {
        /// The declaration as written.
        decl: &'a ClassDecl,
        /// The file whose imports resolve its fields and methods.
        source: SourceId,
    },
}

impl GenericAggregate<'_> {
    /// The declaration's type-parameter list.
    pub(crate) fn type_params(&self) -> &[kira_syntax_model::ast::TypeParamDecl] {
        match self {
            Self::Struct { decl, .. } => &decl.type_params,
            Self::Class { decl, .. } => &decl.type_params,
        }
    }

    /// The declaration's source file.
    pub(crate) fn source(&self) -> SourceId {
        match self {
            Self::Struct { source, .. } | Self::Class { source, .. } => *source,
        }
    }
}

/// Every generic struct or class template, keyed by its bare source name.
pub(crate) type GenericAggregateTable<'a> = HashMap<String, GenericAggregate<'a>>;

/// A generic free-function declaration. Methods use the aggregate table's
/// callable path, while this table is what lets a call infer a free function's
/// parameters before a concrete signature exists.
#[derive(Debug, Clone, Copy)]
pub(crate) struct GenericFunction<'a> {
    /// The declaration as written.
    pub(crate) function: &'a Function,
    /// The source file whose imports resolve the declaration.
    pub(crate) source: SourceId,
}

/// Every generic free function template, keyed by its written name.
pub(crate) type GenericFunctionTable<'a> = HashMap<String, GenericFunction<'a>>;

impl<'a> Analyzer<'a> {
    /// Registers generic declarations before the ordinary type and callable
    /// passes. A template is deliberately absent from the concrete tables: its
    /// name becomes a type or function only after a use site supplies all of
    /// its arguments.
    pub(crate) fn collect_generic_declarations(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            self.source = source;
            match item {
                Item::Struct(decl) if !decl.type_params.is_empty() => {
                    let name = self.interner.resolve(decl.name).to_owned();
                    self.validate_type_params(&name, &decl.type_params);
                    if self.generic_aggregates.contains_key(&name)
                        || self.generic_enums.contains_key(&name)
                    {
                        self.emit(
                            decl.name_span,
                            "KSEM169",
                            format!("generic declaration `{name}` is already defined"),
                        );
                    } else {
                        self.generic_aggregates
                            .insert(name, GenericAggregate::Struct { decl, source });
                    }
                }
                Item::Class(decl) if !decl.type_params.is_empty() => {
                    let name = self.interner.resolve(decl.name).to_owned();
                    self.validate_type_params(&name, &decl.type_params);
                    if self.generic_aggregates.contains_key(&name)
                        || self.generic_enums.contains_key(&name)
                    {
                        self.emit(
                            decl.name_span,
                            "KSEM169",
                            format!("generic declaration `{name}` is already defined"),
                        );
                    } else {
                        self.generic_aggregates
                            .insert(name, GenericAggregate::Class { decl, source });
                    }
                }
                Item::Function(function) if !function.type_params.is_empty() => {
                    let name = self.interner.resolve(function.name).to_owned();
                    self.validate_type_params(&name, &function.type_params);
                    if self.generic_functions.contains_key(&name) {
                        self.emit(
                            function.name_span,
                            "KSEM003",
                            format!("function `{name}` is already defined"),
                        );
                    } else {
                        self.generic_functions
                            .insert(name, GenericFunction { function, source });
                    }
                }
                Item::Trait(declaration) if !declaration.type_params.is_empty() => {
                    let name = self.interner.resolve(declaration.name).to_owned();
                    self.validate_type_params(&name, &declaration.type_params);
                }
                _ => {}
            }
        }
    }

    /// Checks the declaration-local part of a generic parameter list. Trait
    /// existence is checked after all trait declarations are collected, while
    /// duplicate parameters and builtin shadowing can be diagnosed immediately.
    fn validate_type_params(
        &mut self,
        owner: &str,
        params: &[kira_syntax_model::ast::TypeParamDecl],
    ) {
        let mut seen = HashSet::new();
        for param in params {
            let name = self.interner.resolve(param.name).to_owned();
            if Type::from_name(&name).is_some() {
                self.emit(
                    param.span,
                    "KSEM170",
                    format!("type parameter `{name}` of `{owner}` shadows a builtin type"),
                );
            }
            if !seen.insert(name.clone()) {
                self.emit(
                    param.span,
                    "KSEM171",
                    format!("`{owner}` already declares a type parameter `{name}`"),
                );
            }
            for bound in &param.bounds {
                let bound_name = self.interner.resolve(bound.name).to_owned();
                if !is_builtin_trait(&bound_name) && !self.traits.contains_key(&bound_name) {
                    self.emit(
                        bound.span,
                        "KSEM289",
                        format!("`{bound_name}` is not a trait, so it cannot bound `{name}`"),
                    );
                }
            }
        }
    }

    /// Whether a name is a generic struct or class template.
    pub(crate) fn is_generic_aggregate(&self, name: &str) -> bool {
        self.generic_aggregates.contains_key(name)
    }

    /// Whether a name is a generic free-function template.
    pub(crate) fn is_generic_function(&self, name: &str) -> bool {
        self.generic_functions.contains_key(name)
    }

    /// Whether `name` denotes a generic trait template. Trait templates live
    /// in the ordinary trait table so their declaration remains the single
    /// source of member bodies; concrete instances are added beside it under
    /// their mangled names.
    pub(crate) fn is_generic_trait(&self, name: &str) -> bool {
        self.traits
            .get(name)
            .is_some_and(|trait_info| !trait_info.type_params.is_empty())
    }

    /// Resolves a trait reference in a conformance or bound clause to the
    /// concrete trait row it names. A parameterized trait is a contract
    /// template just like a parameterized enum is a value template: its
    /// arguments are resolved in the current type-binding frame and the
    /// resulting `TraitInfo` is memoized under `Trait<Args>`.
    pub(crate) fn resolve_trait_ref(&mut self, reference: &TraitRef) -> Option<String> {
        let base = self.interner.resolve(reference.name).to_owned();
        self.resolve_trait_parts(&base, &reference.args, reference.span)
    }

    /// Resolves a trait name whose syntax was already stored as text (as in a
    /// concrete instance's supertrait list), avoiding a second interner just to
    /// reconstruct an AST symbol.
    fn resolve_trait_parts(
        &mut self,
        base: &str,
        arguments: &[TypeRefId],
        span: Span,
    ) -> Option<String> {
        if is_builtin_trait(base) {
            if !arguments.is_empty() {
                self.emit(
                    span,
                    "KSEM222",
                    format!("trait `{base}` does not take explicit type arguments"),
                );
                return None;
            }
            return Some(base.to_owned());
        }
        let Some(template) = self.traits.get(base).cloned() else {
            return None;
        };
        if template.type_params.is_empty() {
            if !arguments.is_empty() {
                self.emit(
                    span,
                    "KSEM222",
                    format!("trait `{base}` does not take explicit type arguments"),
                );
                return None;
            }
            return Some(base.to_owned());
        }
        let args: Vec<Type> = arguments
            .iter()
            .map(|&arg| self.resolve_type_ref(arg))
            .collect();
        let has_error = args.iter().any(|ty| *ty == Type::Error);
        let arity = template.type_params.len();
        if args.len() != arity {
            self.emit(
                span,
                "KSEM174",
                format!(
                    "generic trait `{base}` takes {arity} type argument{}, but {} {} written",
                    if arity == 1 { "" } else { "s" },
                    args.len(),
                    if args.len() == 1 { "was" } else { "were" },
                ),
            );
            return None;
        }
        if has_error {
            return None;
        }
        let key = self.mangle(&base, &args);
        if !self.traits.contains_key(&key) {
            self.instantiate_trait(base, &key, &template, &args, span);
        }
        if template
            .type_params
            .iter()
            .any(|param| !param.bounds.is_empty())
            && !self.pending_bounds.iter().any(|entry| entry.key == key)
        {
            self.pending_bounds.push(PendingBoundCheck {
                key: key.clone(),
                template: base.to_owned(),
                args: args.clone(),
                source: self.source,
                declaration_source: template.source,
                span,
                kind: "trait",
            });
        }
        self.traits.contains_key(&key).then_some(key)
    }

    /// Mints one concrete trait contract from `template`, substituting its
    /// parameters into member signatures and supertrait references. Trait
    /// instances are semantic rows only: no backend or runtime representation
    /// is needed, and all members still point at the source function bodies.
    fn instantiate_trait(
        &mut self,
        base: &str,
        key: &str,
        template: &crate::traits::TraitInfo<'a>,
        args: &[Type],
        span: Span,
    ) {
        if self.instantiation_depth >= MAX_INSTANTIATION_DEPTH {
            self.emit(
                span,
                "KSEM175",
                format!(
                    "generic trait `{base}` instantiates itself without end (gave up at `{key}`)"
                ),
            );
            return;
        }
        let bindings: TypeBindings = template
            .type_params
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .zip(args.iter().copied())
            .collect();
        let outer_bindings = std::mem::replace(&mut self.type_bindings, bindings.clone());
        let outer_source = self.source;
        self.source = template.source;
        self.instantiation_depth += 1;
        let mut instance = template.clone();
        instance.type_params = Vec::new();
        instance.type_bindings = bindings;
        let supertraits = template
            .supertraits
            .iter()
            .filter_map(|supertrait| {
                self.resolve_trait_parts(&supertrait.name, &supertrait.args, supertrait.span)
                    .map(|name| crate::traits::SupertraitRef {
                        name,
                        span: supertrait.span,
                        args: Vec::new(),
                    })
            })
            .collect();
        instance.supertraits = supertraits;
        self.instantiation_depth -= 1;
        self.source = outer_source;
        self.type_bindings = outer_bindings;
        self.traits.insert(key.to_owned(), instance);
    }

    /// Reports `Name<...>` where `Name` is not a generic enum, saying which of
    /// the two mistakes it is.
    fn report_not_generic(&mut self, text: &str, name_span: Span, span: Span) {
        let known = Type::from_name(text).is_some()
            || self.visible_enum(text).is_some()
            || self.visible_struct(text).is_some()
            || self.aliases.contains_key(text);
        if known {
            self.emit(
                span,
                "KSEM173",
                format!("`{text}` is not generic, so it takes no type arguments"),
            );
        } else {
            self.emit(
                name_span,
                "KSEM050",
                format!("unknown generic type `{text}`"),
            );
        }
    }

    /// Substitutes `args` into `template` and declares the result, or returns
    /// the row an earlier writing of the same instantiation already declared.
    fn instantiate(
        &mut self,
        text: &str,
        template: GenericEnum<'a>,
        args: &[Type],
        span: Span,
    ) -> Type {
        let mangled = self.mangle(text, args);
        if let Some(id) = self.program.types.enums().lookup(&mangled) {
            return Type::Enum(id);
        }
        if self.instantiation_depth >= MAX_INSTANTIATION_DEPTH {
            self.emit(
                span,
                "KSEM175",
                format!(
                    "generic enum `{text}` instantiates itself without end (gave up at \
                     `{mangled}`); a template's payload may not grow its own type argument"
                ),
            );
            return Type::Error;
        }

        // A bounded parameter's obligation belongs to this site, but the answer
        // does not exist yet: an instantiation is minted wherever a type
        // resolves, which can be long before the conformance table and the drop
        // facts are final. Queued under the mangled name — the same key the
        // memo above answers — so one row is checked exactly once.
        if template
            .decl
            .type_params
            .iter()
            .any(|param| !param.bounds.is_empty())
        {
            let fresh = !self.pending_bounds.iter().any(|entry| entry.key == mangled);
            if fresh {
                self.pending_bounds.push(PendingBoundCheck {
                    key: mangled.clone(),
                    template: text.to_owned(),
                    args: args.to_vec(),
                    source: self.source,
                    declaration_source: template.source,
                    span,
                    kind: "enum",
                });
            }
        }

        let bindings: TypeBindings = template
            .decl
            .type_params
            .iter()
            .map(|param| self.interner.resolve(param.name).to_owned())
            .zip(args.iter().copied())
            .collect();

        // A template's body resolves against the file that wrote it and against
        // its own parameters alone; the use site's frame is set aside for the
        // duration so an outer parameter cannot leak in.
        let outer_bindings = std::mem::replace(&mut self.type_bindings, bindings);
        let outer_source = self.source;
        let outer_blame = self.payload_blame.replace((outer_source, span));
        self.source = template.source;
        self.instantiation_depth += 1;

        let (def, defaults) = self.resolve_enum_def(template.decl, mangled);

        self.instantiation_depth -= 1;
        self.source = outer_source;
        self.payload_blame = outer_blame;
        self.type_bindings = outer_bindings;

        match self.program.types.enums_mut().declare(def) {
            // Pushed only on success, which keeps `enum_defaults` indexed by
            // the same ids the table mints.
            Some(id) => {
                self.enum_defaults.push(defaults);
                // Remembering what minted this row is what lets a later
                // `Result.Ok(1)` recognize its own instantiation, and what lets
                // `Result<Int, E>` widen into `Result<Any, E>`; the mangled name
                // spells the arguments, so neither can be read back off it
                // without parsing what was printed. It is recorded in the enum
                // table rather than beside the analyzer because the widening
                // rule is asked of the program's types long after analysis is
                // over — see [`kira_semantics_model::TypeTable::admits`].
                self.program.types.enums_mut().record_instantiation(
                    id,
                    Instantiation {
                        template: text.to_owned(),
                        arguments: args.to_vec(),
                    },
                );
                Type::Enum(id)
            }
            // Unreachable in practice — the memo above already returned for a
            // name the table holds — but a wrong answer is worse than an error.
            None => Type::Error,
        }
    }

    /// The name an instantiation is declared under: the template's name with
    /// its arguments spelled out, e.g. `Result<Int, AppError>`.
    ///
    /// This is both the memo key and what every diagnostic prints, which is why
    /// it is spelled the way the source writes it rather than encoded.
    ///
    /// A nominal argument is spelled with its **owner** — `Lib::Point`, not
    /// `Point` — because two packages may each declare a `Point`, and a
    /// display-only spelling would let `Boxed<Point>` built on one package's
    /// struct be reused as the other's: no diagnostic, and payload type
    /// confusion at the erased boundary.
    fn mangle(&self, text: &str, args: &[Type]) -> String {
        let mut mangled = String::with_capacity(text.len() + 8 * args.len());
        mangled.push_str(text);
        mangled.push('<');
        for (index, &arg) in args.iter().enumerate() {
            if index > 0 {
                mangled.push_str(", ");
            }
            mangled.push_str(&self.identity_spelling(arg));
        }
        mangled.push('>');
        mangled
    }

    /// How `ty` is spelled when identity matters.
    ///
    /// Scalars and pointers have one spelling each; a nominal type repeats its
    /// name across packages by design, so its owner travels with it. An
    /// instantiation row already spells its own arguments' identities in its
    /// name, so reusing the name is sound.
    fn identity_spelling(&self, ty: Type) -> String {
        match ty {
            Type::Struct(id) => match self.program.types.structs().owner_of(id) {
                Some(owner) => format!("{owner}::{}", self.program.types.type_name(ty)),
                None => self.program.types.type_name(ty),
            },
            Type::Enum(id) => {
                let name = self.program.types.type_name(ty);
                match self.program.types.enums().owner_of(id) {
                    Some(owner) => format!("{owner}::{name}"),
                    None => name,
                }
            }
            other => self.type_name(other),
        }
    }
}

/// Every generic enum a program declares, keyed by name.
pub(crate) type GenericEnumTable<'a> = HashMap<String, GenericEnum<'a>>;
