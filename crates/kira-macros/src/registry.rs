//! Finding every `macro` and `comptime macro` declaration in a program, and
//! the model of what one says.
//!
//! Finding them is a per-file question: [`collect_file`] reads one file and
//! nothing else, and [`Registry::absorb`] merges the results in file order.
//! Splitting it that way is what lets the frontend memoize the scan of a
//! dependency that has not changed instead of redoing it every compilation.
//!
//! Using them is a per-file question too. The merge is program-wide because a
//! name declared twice in one scope is, and because the order two packages
//! resolve in is the merge order — but a macro is a name a file writes, so
//! [`Registry::visible_to`] hands each file the declarations its own imports
//! reach and nothing further.
//!
//! [`model`] is what a declaration says, [`scan`] is how one is read off a
//! token stream, and this module is what a program does with them.

use std::collections::{HashMap, HashSet};

use kira_diagnostics::Diagnostic;
use kira_source::{SourceId, Span};

use crate::diagnostics;

pub(crate) mod model;
mod scan;
#[cfg(test)]
mod scan_tests;

pub(crate) use model::{
    ComptimeFunction, Declarative, Fragment, FragmentKind, Procedural, ProceduralKind, kind_word,
};
pub(crate) use scan::collect_file;

/// Every macro a program declares.
#[derive(Debug, Default, Clone, PartialEq)]
pub(crate) struct Registry {
    declarative: HashMap<String, Declarative>,
    procedural: HashMap<String, Procedural>,
    comptime_functions: HashMap<String, ComptimeFunction>,
    enums: HashMap<String, Vec<String>>,
    /// Which package declared the macro now holding each name.
    ///
    /// A name is one declaration inside one scope, and this is what says
    /// whether the declaration about to take a name is in the same scope as
    /// the one already holding it.
    owners: HashMap<String, Option<String>>,
}

impl Registry {
    /// Adds one file's declarations, reporting a name its own scope already
    /// declared and letting a nearer package's win over a further one's.
    ///
    /// Files arrive dependencies first and the program's own last, so a later
    /// name winning is the resolution order the rest of the language uses:
    /// the program's own package, then the packages it imports. Between two
    /// *different* packages that is a deliberate override and silent.
    ///
    /// Inside one package it is not an override, it is the same name declared
    /// twice — and answering it by file order means the program's behaviour
    /// depends on which file was read first, which is the failure that cannot
    /// be reproduced from the source. Reported instead, the way `KSEM004`
    /// reports a type declared twice in one package.
    pub(crate) fn absorb(
        &mut self,
        owner: Option<&str>,
        file: &FileRegistry,
        conflicts: &mut Vec<Diagnostic>,
    ) {
        for declared in &file.declarative {
            if self.claim(
                &declared.name,
                owner,
                declared.source,
                declared.span,
                conflicts,
            ) {
                self.declarative
                    .insert(declared.name.clone(), declared.clone());
            }
        }
        for declared in &file.procedural {
            if self.claim(
                &declared.name,
                owner,
                declared.source,
                declared.span,
                conflicts,
            ) {
                self.procedural
                    .insert(declared.name.clone(), declared.clone());
            }
        }
        for declared in &file.comptime_functions {
            if self.claim(
                &declared.name,
                owner,
                declared.source,
                declared.span,
                conflicts,
            ) {
                self.comptime_functions
                    .insert(declared.name.clone(), declared.clone());
            }
        }
        for (name, variants) in &file.enums {
            self.enums.insert(name.clone(), variants.clone());
        }
    }

    /// Records `name` as declared by `owner`, answering whether the
    /// declaration takes the name.
    ///
    /// It does not when its own scope already declared that name: the first
    /// declaration keeps it, so which file was read first cannot change what
    /// the program means, and the second is reported.
    fn claim(
        &mut self,
        name: &str,
        owner: Option<&str>,
        source: SourceId,
        span: Span,
        conflicts: &mut Vec<Diagnostic>,
    ) -> bool {
        if let Some(held) = self.owners.get(name)
            && held.as_deref() == owner
        {
            let scope = match owner {
                Some(package) => format!("package `{package}`"),
                None => "this program".to_owned(),
            };
            conflicts.push(diagnostics::error(
                source,
                span,
                diagnostics::DUPLICATE_MACRO,
                format!(
                    "macro `{name}` is already declared in {scope}: a macro name means exactly \
                     one declaration, and answering a second by file order would make the \
                     program mean whichever file was read first"
                ),
            ));
            return false;
        }
        // A name is one declaration, so the one being shadowed leaves every
        // kind and not just the one the new declaration happens to be. A
        // declarative `Name` taking the name from a dependency's attribute
        // macro otherwise leaves that attribute reachable as `@Name`: the
        // lookups are by kind, and only the kind map that was written to would
        // have been corrected.
        if self
            .owners
            .insert(name.to_owned(), owner.map(str::to_owned))
            .is_some()
        {
            self.declarative.remove(name);
            self.procedural.remove(name);
            self.comptime_functions.remove(name);
        }
        true
    }

    /// This registry with only the declarations `visible` names left in it.
    ///
    /// A macro is a name a file writes, so it is gated exactly as every other
    /// name a file writes is: the program's own flat scope, the file's own
    /// package, and the packages the file imports — and nothing further,
    /// because visibility does not compose. Merging every package's macros
    /// into one environment made a dependency's dependency's macros callable
    /// from an application that never imported it.
    ///
    /// Enum cases are not filtered. They are not macro names: they are what an
    /// evaluator needs to read `Backend.Glsl` in a template that is already
    /// visible, and the template's own visibility is what decides whether it
    /// runs at all.
    pub(crate) fn visible_to(&self, visible: &HashSet<SourceId>) -> Registry {
        Registry {
            declarative: retain_visible(&self.declarative, visible, |item| item.source),
            procedural: retain_visible(&self.procedural, visible, |item| item.source),
            comptime_functions: retain_visible(&self.comptime_functions, visible, |item| {
                item.source
            }),
            enums: self.enums.clone(),
            owners: self.owners.clone(),
        }
    }

    /// Whether the program declares no macros at all.
    ///
    /// The whole expansion pass is skipped when this holds, which is what keeps
    /// a program that never mentions a macro byte-identical to its own source.
    pub(crate) fn is_empty(&self) -> bool {
        self.declarative.is_empty()
            && self.procedural.is_empty()
            && self.comptime_functions.is_empty()
    }

    /// Every enum the program declares, by name, with its case names.
    ///
    /// A macro body naming `Backend.Glsl` is asking about a type the *program*
    /// declares, not one the compiler knows, so the evaluator has to be told
    /// what the program said.
    pub(crate) fn enums(&self) -> &HashMap<String, Vec<String>> {
        &self.enums
    }

    /// Every `comptime function` the program declares, for the evaluator.
    pub(crate) fn comptime_functions(&self) -> &HashMap<String, ComptimeFunction> {
        &self.comptime_functions
    }

    /// The `comptime function` named `name`, if there is one.
    pub(crate) fn comptime_function(&self, name: &str) -> Option<&ComptimeFunction> {
        self.comptime_functions.get(name)
    }

    /// Every `comptime function` name the program declares, in name order.
    ///
    /// A call to one is found by name alone — it wears no `!` — so the finder
    /// needs the whole set before it can tell one from an ordinary call.
    pub(crate) fn comptime_function_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.comptime_functions.keys().cloned().collect();
        names.sort();
        names
    }

    /// The declarative macro named `name`, if there is one.
    pub(crate) fn declarative(&self, name: &str) -> Option<&Declarative> {
        self.declarative.get(name)
    }

    /// The procedural macro named `name`, if there is one.
    pub(crate) fn procedural(&self, name: &str) -> Option<&Procedural> {
        self.procedural.get(name)
    }

    /// Every procedural macro of one kind, in name order.
    ///
    /// Sorted rather than in hash order so a program with two collectors
    /// appends their files in an order that does not change between runs.
    pub(crate) fn of_kind(&self, kind: ProceduralKind) -> Vec<&Procedural> {
        let mut found: Vec<&Procedural> = self
            .procedural
            .values()
            .filter(|declared| declared.kind == kind)
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        found
    }
}

/// Every macro **one file** declares, in declaration order.
///
/// Per file rather than per program because what a file declares depends on
/// nothing but that file's own bytes. That is what lets a caller memoize the
/// scan and pay for a dependency's macros once rather than once per
/// compilation; [`Registry::absorb`] puts the pieces back together in file
/// order.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct FileRegistry {
    /// The declarative macros this file declares, in declaration order.
    pub(crate) declarative: Vec<Declarative>,
    /// The procedural macros this file declares, in declaration order.
    pub(crate) procedural: Vec<Procedural>,
    /// The `comptime function`s this file declares, in declaration order.
    pub(crate) comptime_functions: Vec<ComptimeFunction>,
    /// Each `enum Name { … }` this file declares, with its case names.
    pub(crate) enums: Vec<(String, Vec<String>)>,
    /// The bytes each declaration covers, `macro` keyword through closing
    /// brace, so the caller can blank them.
    pub(crate) spans: Vec<Span>,
}

impl FileRegistry {
    /// Whether this file declares no macro at all.
    pub(crate) fn is_empty(&self) -> bool {
        self.declarative.is_empty()
            && self.procedural.is_empty()
            && self.comptime_functions.is_empty()
    }
}

/// The entries of `declarations` declared in a file `visible` names.
fn retain_visible<T: Clone>(
    declarations: &HashMap<String, T>,
    visible: &HashSet<SourceId>,
    source_of: impl Fn(&T) -> SourceId,
) -> HashMap<String, T> {
    declarations
        .iter()
        .filter(|(_, declared)| visible.contains(&source_of(declared)))
        .map(|(name, declared)| (name.clone(), declared.clone()))
        .collect()
}
