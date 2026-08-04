//! Selecting what a library's headers contribute, and walking clang's cursors
//! to build a [`BindingModule`] out of it.
//!
//! # What is selected
//!
//! **Functions** come from the headers the `autobind` declaration lists, and
//! from nowhere else. A header includes `<stdio.h>` to get a `FILE *`, and a
//! library that bound every function reachable that way would declare the C
//! standard library into every package that used it — one `@FFI.Extern` per C
//! symbol, program-wide, is the seam's rule, so two libraries doing that would
//! collide on `fread`. `AllPublic` means every function *these headers*
//! declare; `Selected` narrows it to the names the declaration writes.
//!
//! **Types** come from two places: every type reachable from a bound function's
//! signature, at any depth, and — under `AllPublic` — every struct the listed
//! headers define. A type is declared once no matter how many signatures reach
//! it.
//!
//! # What is skipped
//!
//! A declaration the seam cannot carry is recorded with its reason rather than
//! dropped, and the reasons are written into the generated file. That is the
//! difference between a binding that is missing a function and a binding that
//! says why: a variadic C function has no fixed signature, so no `@FFI.Extern`
//! could describe it, and a hand-written one would be refused by the same rule.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use kira_clang::{Cursor, CursorKind, TranslationUnit};
use kira_native_lib_definition::{AutobindMode, AutobindSpec};

use super::model::{BindingModule, FunctionDecl, ParamDecl, SkippedDecl};

/// The walk's state: what was asked for, and what has been built so far.
pub(super) struct Harvest {
    /// The headers whose declarations are this library's own.
    pub(super) headers: HashSet<PathBuf>,
    /// How much of them to expose.
    pub(super) mode: AutobindMode,
    /// The functions a `Selected` declaration names.
    pub(super) functions: HashSet<String>,
    /// The structs a `Selected` declaration names.
    pub(super) structs: HashSet<String>,
    /// What has been built.
    pub(super) module: BindingModule,
    /// Every type name already declared, so a type reached twice is declared
    /// once.
    pub(super) declared: HashSet<String>,
    /// Types whose declaration is in progress, which stops a cycle.
    pub(super) in_progress: HashSet<String>,
    /// Why a named type could not be declared, remembered so a second use does
    /// not re-derive it.
    pub(super) refused: HashMap<String, String>,
}

/// Harvests one library's binding module out of a parsed translation unit.
///
/// `headers` are the absolute paths of the headers the declaration listed; a
/// declaration is this library's only when it was written in one of them.
pub(super) fn harvest(
    unit: &TranslationUnit<'_>,
    library: &str,
    spec: &AutobindSpec,
    headers: &[PathBuf],
) -> BindingModule {
    let mut harvest = Harvest {
        headers: headers.iter().map(|path| canonical(path)).collect(),
        mode: spec.mode,
        functions: spec.functions.iter().cloned().collect(),
        structs: spec.structs.iter().cloned().collect(),
        module: BindingModule {
            library: library.to_owned(),
            ..BindingModule::default()
        },
        declared: HashSet::new(),
        in_progress: HashSet::new(),
        refused: HashMap::new(),
    };

    for cursor in unit.declarations() {
        if !harvest.is_own(&cursor) {
            continue;
        }
        match cursor.kind() {
            CursorKind::FUNCTION_DECL => harvest.take_function(&cursor),
            CursorKind::STRUCT_DECL | CursorKind::TYPEDEF_DECL => harvest.take_type(&cursor),
            _ => {}
        }
    }

    // A name the declaration asked for and the headers never declared is the
    // one selection mistake worth naming: everything else it asked for arrived.
    harvest.report_missing_selections();
    harvest.module.sort();
    harvest.module
}

impl Harvest {
    /// Whether a cursor was written in one of the listed headers.
    fn is_own(&self, cursor: &Cursor<'_>) -> bool {
        let file = cursor.file();
        !file.is_empty() && self.headers.contains(&canonical(Path::new(&file)))
    }

    /// Whether this declaration's mode and selection include `name`.
    fn selects_function(&self, name: &str) -> bool {
        match self.mode {
            AutobindMode::AllPublic => true,
            AutobindMode::Selected => self.functions.contains(name),
        }
    }

    /// Whether this declaration's mode and selection include struct `name`.
    fn selects_struct(&self, name: &str) -> bool {
        match self.mode {
            AutobindMode::AllPublic => true,
            AutobindMode::Selected => self.structs.contains(name),
        }
    }

    /// Binds one C function, or records why it could not be bound.
    fn take_function(&mut self, cursor: &Cursor<'_>) {
        let symbol = cursor.name();
        if symbol.is_empty() || !self.selects_function(&symbol) {
            return;
        }
        // A definition inside a header is an inline or `static` helper: it has
        // no symbol in the archive, so binding it would produce a link failure
        // rather than a call.
        if cursor.is_static() || cursor.is_definition() {
            return;
        }
        if self
            .module
            .functions
            .iter()
            .any(|bound| bound.symbol == symbol)
        {
            return;
        }
        if let Some(reason) = super::names::unbindable_name(&symbol) {
            self.skip(&symbol, reason);
            return;
        }
        if cursor.is_variadic() {
            self.skip(
                &symbol,
                "a variadic C function has no fixed signature to bind".to_owned(),
            );
            return;
        }

        let result = match self.map_result(&cursor.result_type()) {
            Ok(result) => result,
            Err(reason) => {
                self.skip(&symbol, format!("its result is {reason}"));
                return;
            }
        };
        let mut params = Vec::new();
        for (index, parameter) in cursor
            .children()
            .iter()
            .filter(|child| child.kind() == CursorKind::PARM_DECL)
            .enumerate()
        {
            let param_type = match self.map_parameter(&parameter.c_type()) {
                Ok(mapped) => mapped,
                Err(reason) => {
                    self.skip(&symbol, format!("parameter {index} is {reason}"));
                    return;
                }
            };
            params.push(ParamDecl {
                name: super::names::parameter_name(&parameter.name(), index),
                param_type,
            });
        }

        self.module.functions.push(FunctionDecl {
            symbol,
            params,
            result,
        });
    }

    /// Declares one struct or typedef the headers define, when it is selected.
    fn take_type(&mut self, cursor: &Cursor<'_>) {
        if self.mode == AutobindMode::Selected && self.structs.is_empty() {
            return;
        }
        let declared = match cursor.kind() {
            CursorKind::TYPEDEF_DECL => cursor.typedef_underlying_type(),
            _ => cursor.c_type(),
        };
        let Some(name) = self.type_name(&declared) else {
            return;
        };
        if !self.selects_struct(&name) {
            return;
        }
        // A C type the headers name and never define is opaque on purpose —
        // `typedef struct demo_engine demo_engine;` is how a library hides its
        // layout. It is declared as an opaque alias, not written down as
        // something the seam failed to carry.
        if self.declare_if_opaque(&declared, &name) {
            return;
        }
        if let Err(reason) = self.declare_named(&declared) {
            self.skip(&name, reason);
        }
    }

    /// Records one declaration this seam cannot carry.
    pub(super) fn skip(&mut self, name: &str, reason: String) {
        if self
            .module
            .skipped
            .iter()
            .any(|already| already.name == name)
        {
            return;
        }
        self.module.skipped.push(SkippedDecl {
            name: name.to_owned(),
            reason,
        });
    }

    /// Records every name a `Selected` declaration asked for and never got.
    fn report_missing_selections(&mut self) {
        let missing: Vec<String> = self
            .functions
            .iter()
            .filter(|name| {
                !self
                    .module
                    .functions
                    .iter()
                    .any(|bound| &&bound.symbol == name)
                    && !self.module.skipped.iter().any(|s| &&s.name == name)
            })
            .cloned()
            .collect();
        for name in missing {
            self.skip(
                &name,
                "named by the `autobind` declaration and not declared by its headers".to_owned(),
            );
        }
        let missing: Vec<String> = self
            .structs
            .iter()
            .filter(|name| !self.declared.contains(*name) && !self.refused.contains_key(*name))
            .cloned()
            .collect();
        for name in missing {
            self.skip(
                &name,
                "named by the `autobind` declaration and not defined by its headers".to_owned(),
            );
        }
    }
}

/// A path in the one spelling two references to the same file share.
///
/// Header paths arrive twice — once as the declaration wrote them, once as
/// clang reports the file a cursor came from — and a symlinked or `..`-relative
/// spelling would make the two look like different files, which silently binds
/// nothing.
fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf())
}
