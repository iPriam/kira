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
use kira_syntax_model::ast::{Item, TypeRefId};

use crate::analyze::Analyzer;
use crate::types::NameContext;

/// One `type Name = Target` declaration, plus where its resolution stands.
#[derive(Debug, Clone, Copy)]
pub(crate) struct AliasHeader {
    /// The written target type.
    target: TypeRefId,
    /// Span of the alias name, where a cycle is reported.
    name_span: Span,
    /// How far this alias has got through resolution.
    state: AliasState,
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
            let Item::TypeAlias(declaration) = item else {
                continue;
            };
            // An alias is written in one file, so its diagnostics point there.
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            if let Some(what) = self.alias_name_collision(&name) {
                self.emit(
                    declaration.name_span,
                    "KSEM130",
                    format!("type alias `{name}` collides with {what}"),
                );
                continue;
            }
            self.aliases.insert(
                name,
                AliasHeader {
                    target: declaration.target,
                    name_span: declaration.name_span,
                    state: AliasState::Unresolved,
                },
            );
        }
    }

    /// What `name` already means, when an alias may not claim it.
    fn alias_name_collision(&self, name: &str) -> Option<String> {
        if Type::from_name(name).is_some() {
            return Some(format!("the builtin type `{name}`"));
        }
        if self.aliases.contains_key(name) {
            return Some(format!("an earlier type alias `{name}`"));
        }
        // The struct and enum tables are empty at this point, so the check runs
        // against the declarations as written.
        for item in self.tree.items() {
            let (kind, declared) = match item {
                Item::Struct(declaration) => ("struct", declaration.name),
                Item::Enum(declaration) => ("enum", declaration.name),
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
    /// Only a *successful* resolution is memoized. An alias whose target does
    /// not resolve stays unresolved, so each use site reports against its own
    /// span through its own [`NameContext`] rather than inheriting whichever
    /// site happened to touch the alias first.
    pub(crate) fn resolve_alias_name(&mut self, name: &str, context: &NameContext) -> Option<Type> {
        let header = *self.aliases.get(name)?;
        match header.state {
            AliasState::Resolved(ty) => return Some(ty),
            AliasState::Resolving => {
                self.set_alias_state(name, AliasState::Unresolved);
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
        let ty = self.resolve_type_in(header.target, context);
        let state = match ty {
            Type::Error => AliasState::Unresolved,
            resolved => AliasState::Resolved(resolved),
        };
        self.set_alias_state(name, state);
        Some(ty)
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
