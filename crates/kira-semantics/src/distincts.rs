//! `distinct Name = Representation`: registering the declarations, resolving
//! the representation, and the two crossings a program writes.
//!
//! The mirror image of [`crate::aliases`], and the two are written beside each
//! other on purpose. An alias binds a second *spelling* to one type, so a value
//! crosses between the names freely and nothing below semantics learns the
//! alias existed. A `distinct` declaration binds a second *type* to one
//! representation, so nothing crosses without being written — and nothing below
//! semantics learns that type existed either, because `kira-ir` rewrites it to
//! the representation before a backend sees the program.
//!
//! Resolution is lazy and memoized for the reason an alias's is: a declaration
//! may name another (`distinct Ticks = Millis`), and declaration order is not a
//! rule here. Laziness needs a cycle guard, so a declaration reached while it is
//! already resolving is `KSEM346` and yields [`Type::Error`].

use std::collections::HashMap;

use kira_semantics_model::distincts::is_representable;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{DistinctId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, Item, TypeRefId};

use crate::analyze::{Analyzer, FnCtx};
use crate::types::NameContext;

/// Every `distinct Name = Representation` declaration, keyed by
/// `"{package}::{name}"` — or the bare name for one of the program's own
/// files — so two packages may each declare the name.
pub(crate) type DistinctTable = HashMap<String, DistinctHeader>;

fn owned_distinct_key(owner: Option<&str>, name: &str) -> String {
    match owner {
        Some(owner) => format!("{owner}::{name}"),
        None => name.to_owned(),
    }
}

/// One `distinct` declaration, plus where its resolution stands.
#[derive(Debug, Clone)]
pub(crate) struct DistinctHeader {
    /// The written representation, resolved lazily against its file's imports.
    representation: TypeRefId,
    /// Span of the declared name, where a cycle and a bad representation are
    /// both reported.
    name_span: Span,
    /// How far this declaration has got through resolution.
    /// The file that declared it, which decides its owning package.
    source: kira_source::SourceId,
    state: DistinctState,
}

/// The four states a declaration passes through.
///
/// [`DistinctState::Resolving`] is the cycle guard: reaching a declaration in
/// this state means the chain came back to where it started.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DistinctState {
    /// Not yet resolved by any use site.
    Unresolved,
    /// Currently resolving; re-entering this declaration is a cycle.
    Resolving,
    /// Resolved to its own nominal type, memoized so later uses are free and,
    /// more importantly, so every use names *one* row of the table.
    Resolved(Type),
    /// Reported broken — a cycle, or a representation the type cannot be built
    /// over. Every later use answers `Type::Error` silently, so the report is
    /// exactly once.
    Failed,
}

impl Analyzer<'_> {
    /// Registers every `distinct Name = Representation` declaration, rejecting
    /// a name that already means something else.
    ///
    /// Registration only. The representation is resolved on first use, which is
    /// what lets one declaration name another written below it and lets both
    /// name a type alias.
    pub(crate) fn collect_distinct_types(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Distinct(declaration) = item else {
                continue;
            };
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            if let Some(what) = self.distinct_name_collision(&name) {
                self.emit(
                    declaration.name_span,
                    "KSEM130",
                    format!("distinct type `{name}` collides with {what}"),
                );
                continue;
            }
            let key = owned_distinct_key(self.imports.package_of(source), &name);
            self.distincts.insert(
                key,
                DistinctHeader {
                    representation: declaration.representation,
                    name_span: declaration.name_span,
                    source,
                    state: DistinctState::Unresolved,
                },
            );
        }
    }

    /// Resolves every registered `distinct` declaration, whether or not a use
    /// site named one.
    ///
    /// A representation the type cannot be built over is a mistake in the
    /// *declaration*, so it is reported where it was written and reported even
    /// when nothing mentions the type yet — a `distinct Label = String` sitting
    /// in a file nobody has wired up is still wrong, and finding out only after
    /// the first use is how a refusal arrives at the wrong moment.
    ///
    /// Run once every nominal table is filled, so a representation naming a
    /// struct is refused for being a struct rather than for not existing yet.
    pub(crate) fn resolve_declared_distinct_types(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Distinct(declaration) = item else {
                continue;
            };
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            self.resolve_distinct_name(&name, &NameContext::Ordinary);
        }
    }

    /// What `name` already means, when a `distinct` declaration may not claim
    /// it.
    ///
    /// The same rule an alias follows and for the same reason: a silently
    /// ignored declaration would keep type-checking as whatever the name
    /// already meant and give a wrong answer instead of an error. The struct
    /// and enum tables do not exist yet at this point, so the check runs
    /// against the declarations as written.
    fn distinct_name_collision(&self, name: &str) -> Option<String> {
        if Type::from_name(name).is_some() {
            return Some(format!("the builtin type `{name}`"));
        }
        if self.distincts.contains_key(&owned_distinct_key(
            self.imports.package_of(self.source),
            name,
        )) {
            return Some(format!("an earlier distinct type `{name}`"));
        }
        if self.visible_alias_key(name).is_some() {
            return Some(format!("the type alias `{name}`"));
        }
        for item in self.tree.items() {
            let (kind, declared) = match item {
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

    /// Resolves `name` as a distinct type, or `None` when no declaration has
    /// that name.
    ///
    /// A success is memoized because it *must* be: the table row is minted here,
    /// and resolving twice would mint two rows and give one written name two
    /// incompatible types. A cycle and a refused representation are memoized
    /// too, so each is reported exactly once.
    /// The key of the `distinct` declaration `name` resolves to from the
    /// current file: the file's own package first, then the program's own
    /// declarations, then the packages the file imports.
    fn visible_distinct_key(&self, name: &str) -> Option<String> {
        let home = owned_distinct_key(self.imports.package_of(self.source), name);
        if self.distincts.contains_key(&home) {
            return Some(home);
        }
        if self.distincts.contains_key(name) {
            return Some(name.to_owned());
        }
        self.imports
            .imported_packages(self.source)
            .into_iter()
            .map(|package| owned_distinct_key(Some(&package), name))
            .find(|key| self.distincts.contains_key(key))
    }

    pub(crate) fn resolve_distinct_name(
        &mut self,
        name: &str,
        context: &NameContext,
    ) -> Option<Type> {
        let key = self.visible_distinct_key(name)?;
        let name = key.as_str();
        let header = self.distincts.get(name)?.clone();
        match header.state {
            DistinctState::Resolved(ty) => return Some(ty),
            DistinctState::Failed => return Some(Type::Error),
            DistinctState::Resolving => {
                self.set_distinct_state(name, DistinctState::Failed);
                self.emit(
                    header.name_span,
                    "KSEM346",
                    format!(
                        "distinct type `{name}` resolves back to itself; break the cycle so \
                         every `distinct Name = Representation` chain reaches a scalar"
                    ),
                );
                return Some(Type::Error);
            }
            DistinctState::Unresolved => {}
        }
        self.set_distinct_state(name, DistinctState::Resolving);
        let written = self.resolve_type_in(header.representation, context);
        // A cycle running through this declaration marked it failed while the
        // resolution above was in flight, and that verdict stands: reverting it
        // to unresolved would let the next use walk the same cycle and report it
        // again, once per mention.
        if self.distinct_state(name) == Some(DistinctState::Failed) {
            return Some(Type::Error);
        }
        // The representation failed to resolve and said so at its own span.
        // Left unresolved rather than failed so a later use site still reports
        // against its own position, exactly as an alias does.
        if written == Type::Error {
            self.set_distinct_state(name, DistinctState::Unresolved);
            return Some(Type::Error);
        }
        // Another distinct type is a legal representation and is flattened by
        // the table, so `distinct Ticks = Millis` is a third type over the same
        // scalar rather than a chain anything below has to walk.
        if !is_representable(written) && !written.is_distinct() {
            let shown = self.type_name(written);
            self.emit(
                header.name_span,
                "KSEM345",
                format!(
                    "`{shown}` cannot be the representation of a `distinct` type: a distinct \
                     type is one scalar word, so its representation is an integer, a float, \
                     `Bool`, `RawPtr`, or another distinct type. Declare a `struct` to give a \
                     name to a shape that owns storage."
                ),
            );
            self.set_distinct_state(name, DistinctState::Failed);
            return Some(Type::Error);
        }
        let owner = self.imports.package_of(header.source).map(str::to_owned);
        let declared = name.rsplit("::").next().unwrap_or(name).to_owned();
        let ty = self
            .program
            .types
            .declare_distinct_owned(owner.as_deref(), declared, written);
        if let Type::Distinct(id) = ty {
            let module = self.imports.module_of(header.source).to_owned();
            self.program.types.distincts_mut().set_module(id, &module);
        }
        let state = match ty {
            Type::Error => DistinctState::Failed,
            resolved => DistinctState::Resolved(resolved),
        };
        self.set_distinct_state(name, state);
        Some(ty)
    }

    /// The distinct type `name` declares, or `None` when it declares none.
    ///
    /// Resolves it if this is the first mention, so a call written before the
    /// declaration is reached by any other path still finds the type.
    pub(crate) fn distinct_named(&mut self, name: &str) -> Option<Type> {
        self.visible_distinct_key(name)?;
        match self.resolve_distinct_name(name, &NameContext::Ordinary) {
            Some(Type::Distinct(id)) => Some(Type::Distinct(id)),
            _ => None,
        }
    }

    /// Type-checks `TabId(value)`: the one way into a distinct type.
    ///
    /// Returns `None` when the call is not a construction at all — the name
    /// declares no distinct type, or a local shadows it — so the caller carries
    /// on to the ordinary call paths. Otherwise this owns the call and every
    /// branch returns `Some`, which is what keeps a mistake in a construction
    /// from also being reported as an undefined function.
    ///
    /// The argument is checked against the *representation*, so `TabId(1)` and
    /// `TabId(U32(count))` both build one and `TabId(bookmark)` does not: a
    /// distinct type is assignable to nothing but itself, and that includes the
    /// argument slot of another distinct type's construction.
    pub(crate) fn analyze_distinct_construction(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[CallArg],
        span: Span,
    ) -> Option<HirExprId> {
        // A local of the same name shadows the type, exactly as it does for a
        // scalar conversion: `TabId(x)` calls the local when one is in scope.
        if ctx.resolve(name).is_some() {
            return None;
        }
        let ty = self.distinct_named(name)?;
        let representation = self.program.types.representation(ty);
        self.link_type_name(name, span);
        let values = Self::argument_values(args);
        let [only] = values.as_slice() else {
            for &value in &values {
                self.analyze_expr(ctx, value);
            }
            self.emit(
                span,
                "KSEM347",
                format!(
                    "`{name}` is a distinct type, so it is built from exactly one value of \
                     `{}`, found {}",
                    self.type_name(representation),
                    values.len()
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        };
        let value = self.analyze_expr(ctx, *only);
        let actual = self.program.expr(value).type_of();
        if actual == Type::Error {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        if !actual.assignable_to(representation) {
            self.emit(
                self.tree.expr(*only).span(),
                "KSEM348",
                format!(
                    "`{name}` is a distinct type over `{}`, so it is built from one of those; \
                     `{}` is a different type. Write `.raw` to take the representation out of \
                     a value that already has one.",
                    self.type_name(representation),
                    self.type_name(actual)
                ),
            );
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        Some(self.program.exprs.alloc(HirExpr::Distinct { value, ty }))
    }

    /// Type-checks `id.raw`: the one way out of a distinct type.
    ///
    /// A distinct type has exactly this one member, so anything else read off
    /// one is refused here rather than falling through to a field lookup that
    /// would report the wrong thing — the same shape `.count` on an array and
    /// `.await` on a task handle take.
    pub(crate) fn analyze_distinct_property(
        &mut self,
        value: HirExprId,
        id: DistinctId,
        name: &str,
        span: Span,
    ) -> HirExprId {
        let ty = self.program.types.distinct_representation(id);
        if name != "raw" {
            let declared = self.type_name(Type::Distinct(id));
            self.emit(
                span,
                "KSEM349",
                format!(
                    "`{declared}` is a distinct type, so it has one member: `raw`, the \
                     `{}` it is. `{name}` is not one of its members.",
                    self.type_name(ty)
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.program.exprs.alloc(HirExpr::Distinct { value, ty })
    }

    fn set_distinct_state(&mut self, name: &str, state: DistinctState) {
        if let Some(header) = self.distincts.get_mut(name) {
            header.state = state;
        }
    }

    fn distinct_state(&self, name: &str) -> Option<DistinctState> {
        self.distincts.get(name).map(|header| header.state)
    }
}
