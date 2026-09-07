//! Type aliases: `type Name = Target`.
//!
//! An alias is a *frontend* fact and nothing else. Analysis resolves the name
//! to the [`Type`] its target stands for, and the HIR — and therefore the IR,
//! the bytecode, both native backends and wasm — sees exactly what it would
//! have seen had the target been written out. No layer below semantics learns
//! aliases exist, which is why this feature costs no opcode and no backend
//! work.
//!
//! Resolution is lazy and memoized, because an alias may name another alias
//! (`type ByteMatrix = [ByteBuffer]`) and declaration order is not a rule for
//! aliases the way it is for struct fields. Laziness needs a cycle guard: an
//! alias caught resolving *while it is already resolving* is reported once as
//! `KSEM157` and yields [`Type::Error`], so `type A = B` / `type B = A`
//! terminates instead of recursing forever.

use std::collections::HashMap;

use kira_semantics_model::Type;
use kira_source::Span;
use kira_syntax_model::ast::{FfiTypeKind, Item, TypeRefId};

use crate::analyze::Analyzer;
use crate::types::NameContext;

/// One alias declaration, plus where its resolution stands.
///
/// Covers `type Name = Target`, `@FFI.Alias { target: Target }` (both
/// [`AliasBody::Written`]), and `@FFI.Pointer { target: Target }`
/// ([`AliasBody::Pointer`], since a native pointer is one machine word whatever
/// it points at).
#[derive(Debug, Clone)]
pub(crate) struct AliasHeader {
    /// What the alias resolves to.
    body: AliasBody,
    /// Span of the alias name, where a cycle is reported.
    name_span: Span,
    /// The file that declared it, which decides its owning package and the
    /// imports its body resolves against.
    source: kira_source::SourceId,
    /// How far this alias has got through resolution.
    state: AliasState,
    /// What a foreign declaration said about the C type it names, when this
    /// alias came from one. `None` for a written `type Name = Target`, which is
    /// a Kira declaration and may be written only once.
    description: Option<String>,
}

/// What an [`AliasHeader`] stands for.
#[derive(Debug, Clone, Copy)]
enum AliasBody {
    /// A written target type, resolved lazily against its file's imports.
    Written(TypeRefId),
    /// A native pointer, for `@FFI.Pointer`. Always [`Type::RawPtr`]: every
    /// native pointer is one machine word, and the target is never resolved —
    /// see [`Analyzer::resolve_alias_name`] for why resolving it would be
    /// actively wrong. What it is called is recorded in
    /// [`Analyzer::pointer_targets`].
    Pointer,
}

/// The three states an alias passes through, in order.
///
/// [`AliasState::Resolving`] is the cycle guard: reaching an alias in this
/// state means the chain came back to where it started.
#[derive(Debug, Clone, Copy, PartialEq)]
enum AliasState {
    /// Not yet resolved by any use site.
    Unresolved,
    /// Currently resolving; re-entering this alias is a cycle.
    Resolving,
    /// Resolved to a concrete type, memoized so later uses are free.
    Resolved(Type),
    /// Reported broken — a cycle, the one failure that names the alias
    /// itself rather than one of its use sites. Every later use answers
    /// `Type::Error` silently, so the report is exactly once.
    Failed,
}

/// `"{package}::{name}"`, or the bare name for one of the program's own files:
/// two packages may each declare a `type Byte = U8`.
fn owned_alias_key(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(owner) => format!("{owner}::{name}"),
        None => name.to_owned(),
    }
}

impl Analyzer<'_> {
    /// Registers every `type Name = Target` declaration, rejecting a name that
    /// already means something else.
    ///
    /// Registration only — nothing is resolved here. A target may name a
    /// struct or an enum, and neither table exists yet at this point; resolving
    /// on first use is what lets an alias name any type the file declares.
    ///
    /// A name that collides is rejected rather than shadowing, because a
    /// silently-ignored `type Int = Float` would type-check as `Int` and give a
    /// wrong answer instead of an error.
    pub(crate) fn collect_type_aliases(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            // An alias is written in one file, so its diagnostics point there.
            self.source = source;
            match item {
                Item::TypeAlias(declaration) => {
                    let name = self.interner.resolve(declaration.name).to_owned();
                    self.register_alias(
                        name,
                        declaration.name_span,
                        AliasBody::Written(declaration.target),
                        None,
                    );
                }
                // `@FFI.Alias`/`@FFI.Pointer` declare a typedef, not a struct:
                // the struct table skips them (`is_alias_shaped`) and they live
                // here instead, sharing the cycle guard and collision check.
                Item::Struct(declaration) => {
                    let body = match &declaration.ffi.as_ref().map(|mark| &mark.kind) {
                        Some(FfiTypeKind::Alias { target }) => match target {
                            Some(target) => AliasBody::Written(*target),
                            None => {
                                self.emit(
                                    declaration.name_span,
                                    "KSEM188",
                                    "`@FFI.Alias` is missing its required `target` type",
                                );
                                continue;
                            }
                        },
                        Some(FfiTypeKind::Pointer { target, .. }) => {
                            // Record what it points at by name, without
                            // resolving anything: a name is enough to find a
                            // C-layout struct later, and resolution here would
                            // report on targets that are meant to be opaque.
                            if let Some(target) = target {
                                let alias = self.interner.resolve(declaration.name).to_owned();
                                let pointee = self.written_type_name(*target);
                                self.pointer_targets.insert(alias, pointee);
                            }
                            AliasBody::Pointer
                        }
                        _ => continue,
                    };
                    // A forward declaration only names a C type; whatever
                    // describes that name is what it means. `typedef union X X;`
                    // ahead of `X`'s own definition is the shape, and registering
                    // the alias would put it in front of the definition.
                    if crate::ffi_types::is_forward_declaration(declaration)
                        && self.has_foreign_definition(declaration.name)
                    {
                        continue;
                    }
                    let name = self.interner.resolve(declaration.name).to_owned();
                    self.register_alias(
                        name,
                        declaration.name_span,
                        body,
                        self.foreign_description(declaration),
                    );
                }
                _ => continue,
            }
        }
    }

    /// Registers one alias, rejecting a name that already means something else.
    ///
    /// `description` is what a foreign declaration says about the C type it
    /// names, when it is one. An earlier alias of the same name describing the
    /// same type is that type arriving twice — autobind writes `@FFI.Pointer {
    /// target: char; ownership: borrowed; } struct char_ptr {}` into every
    /// binding that needs a `char *` — so the repeat is idempotent. A *different*
    /// description under the same name is still one name given two meanings.
    fn register_alias(
        &mut self,
        name: String,
        name_span: Span,
        body: AliasBody,
        description: Option<String>,
    ) {
        let key = owned_alias_key(self.imports.package_of(self.source), &name);
        if let Some(existing) = self.aliases.get(&key)
            && description.is_some()
            && existing.description == description
        {
            return;
        }
        if let Some(what) = self.alias_name_collision(&name) {
            self.emit(
                name_span,
                "KSEM130",
                format!("type alias `{name}` collides with {what}"),
            );
            return;
        }
        self.aliases.insert(
            key,
            AliasHeader {
                body,
                name_span,
                source: self.source,
                state: AliasState::Unresolved,
                description,
            },
        );
    }

    /// The key of the alias `name` resolves to from the current file: the
    /// file's own package first, then the program's own declarations, then
    /// the packages the file imports.
    pub(crate) fn visible_alias_key(&self, name: &str) -> Option<String> {
        let home = owned_alias_key(self.imports.package_of(self.source), name);
        if self.aliases.contains_key(&home) {
            return Some(home);
        }
        if self.aliases.contains_key(name) {
            return Some(name.to_owned());
        }
        self.imports
            .imported_packages(self.source)
            .into_iter()
            .map(|package| owned_alias_key(Some(&package), name))
            .find(|key| self.aliases.contains_key(key))
    }

    /// Whether some declaration in the program describes the C type `name`,
    /// rather than only naming it.
    ///
    /// What decides whether a forward declaration has a definition to yield to.
    /// Compares the resolved text rather than the symbols: a file interns its
    /// own names, so the `SDL_Window` written in a binding and the one written
    /// in the file asking are two symbols for one C type.
    fn has_foreign_definition(&self, name: kira_core::Symbol) -> bool {
        let name = self.interner.resolve(name);
        self.tree.items().iter().any(|item| match item {
            Item::Struct(other) => {
                self.interner.resolve(other.name) == name
                    && crate::ffi_types::is_foreign_definition(other)
            }
            _ => false,
        })
    }

    /// What `name` already means, when an alias may not claim it.
    fn alias_name_collision(&self, name: &str) -> Option<String> {
        if Type::from_name(name).is_some() {
            return Some(format!("the builtin type `{name}`"));
        }
        if self
            .aliases
            .contains_key(&owned_alias_key(self.imports.package_of(self.source), name))
        {
            return Some(format!("an earlier type alias `{name}`"));
        }
        // The struct and enum tables are empty at this point, so the check runs
        // against the declarations as written.
        for item in self.tree.items() {
            let (kind, declared) = match item {
                // A `@FFI.Alias`/`@FFI.Pointer` struct *is* this alias, not a
                // rival declaration of the name — skip it so it does not collide
                // with itself.
                Item::Struct(declaration) if crate::ffi_types::is_alias_shaped(declaration) => {
                    continue;
                }
                Item::Struct(declaration) => ("struct", declaration.name),
                Item::Enum(declaration) => ("enum", declaration.name),
                Item::Class(declaration) => ("class", declaration.name),
                Item::Trait(declaration) => ("trait", declaration.name),
                _ => continue,
            };
            if self.interner.resolve(declared) == name {
                return Some(format!("the {kind} `{name}`"));
            }
        }
        None
    }

    /// Resolves `name` as a type alias, or `None` when no alias has that name.
    ///
    /// A *successful* resolution is memoized, and so is a cycle: both are
    /// properties of the declaration. Only an alias whose target does not
    /// resolve stays unresolved, because that failure is about a use site —
    /// each one reports against its own span through its own [`NameContext`]
    /// rather than inheriting whichever site happened to touch the alias
    /// first.
    pub(crate) fn resolve_alias_name(&mut self, name: &str, context: &NameContext) -> Option<Type> {
        let key = self.visible_alias_key(name)?;
        let name = key.as_str();
        let header = self.aliases.get(name)?.clone();
        match header.state {
            AliasState::Resolved(ty) => return Some(ty),
            AliasState::Failed => return Some(Type::Error),
            AliasState::Resolving => {
                // Reported once, at the alias; the state records that so no
                // later use re-walks the cycle and repeats it.
                self.set_alias_state(name, AliasState::Failed);
                self.emit(
                    header.name_span,
                    "KSEM157",
                    format!(
                        "type alias `{name}` resolves back to itself; break the cycle so \
                         every `type Name = Target` chain reaches a concrete target type"
                    ),
                );
                return Some(Type::Error);
            }
            AliasState::Unresolved => {}
        }
        self.set_alias_state(name, AliasState::Resolving);
        // The body was written in the declaring file, so it resolves against
        // that file's package and imports rather than the use site's.
        let here = self.source;
        self.source = header.source;
        let declared = name.rsplit("::").next().unwrap_or(name).to_owned();
        let ty = match header.body {
            // A pointer is one machine word whatever it points at. The target
            // is looked up silently: generated bindings point at C types nobody
            // declared (`SECURITY_ATTRIBUTES`) and at themselves (an opaque
            // handle typedef), so a target that does not resolve to a C-layout
            // struct is an opaque handle rather than a mistake and stays a plain
            // `RawPtr`. One that does resolve keeps what it points at, which is
            // what lets a field be read through it.
            AliasBody::Pointer => self.foreign_pointer_type(&declared),
            AliasBody::Written(target) => self.resolve_type_in(target, context),
        };
        self.source = here;
        let state = match ty {
            Type::Error => AliasState::Unresolved,
            resolved => AliasState::Resolved(resolved),
        };
        self.set_alias_state(name, state);
        Some(ty)
    }

    /// The pointer type an `@FFI.Pointer` typedef named `alias` resolves to.
    fn foreign_pointer_type(&mut self, alias: &str) -> Type {
        let Some(target) = self.pointer_targets.get(alias).cloned() else {
            return Type::RawPtr;
        };
        let Some(struct_id) = self.visible_struct(&target) else {
            return Type::RawPtr;
        };
        if self.ffi_struct_kind(struct_id) != Some(crate::ffi_types::FfiStructKind::CLayout) {
            return Type::RawPtr;
        }
        self.program.types.foreign_ptr_to(struct_id)
    }

    /// Advances one alias's resolution state.
    fn set_alias_state(&mut self, name: &str, state: AliasState) {
        if let Some(header) = self.aliases.get_mut(name) {
            header.state = state;
        }
    }
}

/// The alias table an [`Analyzer`] carries, keyed by alias name.
pub(crate) type AliasTable = HashMap<String, AliasHeader>;
