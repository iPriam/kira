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
//! # Why only an enum is generic
//!
//! The reference corpus contains exactly one generic declaration, and it is
//! `Result`. A generic struct, class, or function has no call site anywhere, so
//! building one would be speculative surface; the parser refuses those by name
//! instead (`KPAR047`).
//!
//! # Termination
//!
//! Instantiation is recursive: a template's payload may itself be a generic
//! enum. Memoizing on the mangled name stops direct recursion, but
//! `enum Grow<Value> { More(Grow<[Value]>) }` mints a fresh name every time, so a depth
//! cap is what actually terminates it. Hitting the cap is `KSEM175` — a typed
//! refusal, never a stack overflow.

use std::collections::HashMap;

use kira_semantics_model::{EnumId, Instantiation, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{EnumDecl, TypeRefId};

use crate::analyze::Analyzer;
use crate::types::NameContext;

/// How deep a chain of generic instantiations may go before it is refused.
///
/// Nothing legitimate nests this far — the corpus never nests at all — so the
/// cap only ever fires on a template that grows its own argument.
const MAX_INSTANTIATION_DEPTH: u32 = 16;

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

impl<'a> Analyzer<'a> {
    /// Registers `declaration` as a generic template, or reports why it cannot
    /// be one.
    ///
    /// Returns `true` when the declaration was generic — the caller then skips
    /// declaring it as an ordinary enum, because a template names no type.
    pub(crate) fn register_generic_enum(
        &mut self,
        declaration: &'a EnumDecl,
        source: SourceId,
    ) -> bool {
        if declaration.type_params.is_empty() {
            return false;
        }
        let name = self.interner.resolve(declaration.name).to_owned();
        let mut seen: Vec<String> = Vec::with_capacity(declaration.type_params.len());
        for param in &declaration.type_params {
            let param_name = self.interner.resolve(param.name).to_owned();
            if Type::from_name(&param_name).is_some() {
                self.emit(
                    param.span,
                    "KSEM170",
                    format!(
                        "type parameter `{param_name}` of enum `{name}` shadows a builtin type; \
                         pick a name no type already has"
                    ),
                );
                continue;
            }
            if seen.contains(&param_name) {
                self.emit(
                    param.span,
                    "KSEM171",
                    format!("enum `{name}` already declares a type parameter `{param_name}`"),
                );
                continue;
            }
            seen.push(param_name);
        }
        if self.generic_enums.contains_key(&name)
            || self.visible_enum(&name).is_some()
            || self.visible_struct(&name).is_some()
        {
            self.emit(
                declaration.name_span,
                "KSEM169",
                format!("generic enum `{name}` is already defined"),
            );
            return true;
        }
        self.generic_enums.insert(
            name,
            GenericEnum {
                decl: declaration,
                source,
            },
        );
        true
    }

    /// Whether `name` is a registered generic enum, so a bare use of it can say
    /// what is missing rather than "unknown type".
    pub(crate) fn is_generic_enum(&self, name: &str) -> bool {
        self.generic_enums.contains_key(name)
    }

    /// The instantiation of the template `name` that `expected` already asks
    /// for, if that is what it asks for.
    ///
    /// This is what lets `Result.Ok(1)` mean the same thing `.Ok(1)` does: a
    /// qualified spelling carries no type arguments, so the *position* is what
    /// supplies them, and the template name written in front only has to agree
    /// with what the position already said. It agrees when the expected enum is
    /// an instantiation of exactly this template — `Result<Int, Bool>` for
    /// `Result`, and not for `Outcome`.
    pub(crate) fn generic_instantiation_expected(
        &self,
        name: &str,
        expected: Option<Type>,
    ) -> Option<EnumId> {
        let Some(Type::Enum(id)) = expected else {
            return None;
        };
        (self.program.types.enums().template_of(id) == Some(name)).then_some(id)
    }

    /// Resolves the type parameter `name` stands for in the substitution
    /// currently in force, if any.
    pub(crate) fn bound_type_param(&self, name: &str) -> Option<Type> {
        self.type_bindings
            .iter()
            .rev()
            .find(|(bound, _)| bound == name)
            .map(|&(_, ty)| ty)
    }

    /// Resolves a written `Name<Args>` to the enum its instantiation declares.
    pub(crate) fn resolve_generic_instantiation(
        &mut self,
        name: kira_core::Symbol,
        name_span: Span,
        args: &[TypeRefId],
        span: Span,
        context: &NameContext,
    ) -> Type {
        let written = self.interner.resolve(name).to_owned();
        // A generic template is keyed by its bare name, so the qualifier here
        // buys only the file-scope check the split performs.
        let Some(text) = self
            .split_module_qualifier(&written, name_span)
            .map(|qualified| qualified.text)
        else {
            // Reported already; still resolve the arguments so their own
            // mistakes are not hidden behind one unimported module.
            for &arg in args {
                self.resolve_type_in(arg, context);
            }
            return Type::Error;
        };
        // Arguments resolve in the *use site's* scope, which is the current
        // binding frame — that is what makes `Result<Value, E>` inside another
        // template's body mean what it says.
        let mut resolved: Vec<Type> = Vec::with_capacity(args.len());
        let mut any_error = false;
        for &arg in args {
            let ty = self.resolve_type_in(arg, context);
            any_error |= ty == Type::Error;
            resolved.push(ty);
        }
        let Some(template) = self.generic_enums.get(&text).copied() else {
            self.report_not_generic(&text, name_span, span);
            return Type::Error;
        };
        let arity = template.decl.type_params.len();
        if resolved.len() != arity {
            self.emit(
                span,
                "KSEM174",
                format!(
                    "generic enum `{text}` takes {arity} type argument{}, but {} {} written",
                    if arity == 1 { "" } else { "s" },
                    resolved.len(),
                    if resolved.len() == 1 { "was" } else { "were" }
                ),
            );
            return Type::Error;
        }
        // An argument that did not resolve would mint a row named
        // `Result<<error>, E>` that a second unresolved name would compare
        // *equal* to — turning one mistake into two unrelated types that
        // type-check against each other. Same reasoning as `[<error>]`.
        if any_error {
            return Type::Error;
        }
        self.instantiate(&text, template, &resolved, span)
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
