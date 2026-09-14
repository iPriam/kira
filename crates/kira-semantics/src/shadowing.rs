//! Refusing a top-level type name that shadows one an import already provides.
//!
//! Kira has one type namespace, and a name means exactly one declaration. A
//! package that declared its own `Color` while `Foundation` (always in scope)
//! or an imported package already declared one used to keep both: structs are
//! owner-keyed, so the two coexisted and a bare `Color` resolved to the file's
//! own. That is shadowing, and it is the bug `Foundation`'s own `Color` comment
//! describes — two nominal `Color`s read as `expects Color, found Color` the
//! moment one meets an API speaking the other.
//!
//! So a declaration whose name is also provided by something this file imports
//! is refused here, after every type table exists. The escape is to remove the
//! declaration and use the imported type, or to rename this one — never to
//! shadow.

use kira_source::{SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::Item;

use crate::analyze::Analyzer;

impl<'a> Analyzer<'a> {
    /// Reports every top-level type declaration whose name an imported module —
    /// `Foundation` or a package this file imports — already declares.
    ///
    /// Runs once every struct, enum, class, distinct, alias, and construct
    /// family has a row, so "already provided" is a settled question rather than
    /// one that depends on which file this collection pass visited first.
    pub(crate) fn reject_cross_package_shadowing(&mut self) {
        let tree: &'a SyntaxTree = self.tree;
        let mut declared: Vec<(SourceId, kira_core::Symbol, Span)> = Vec::new();
        for (source, item) in tree.items_with_source() {
            let named = match item {
                Item::Struct(decl) => Some((decl.name, decl.name_span)),
                Item::Enum(decl) => Some((decl.name, decl.name_span)),
                Item::Class(decl) => Some((decl.name, decl.name_span)),
                Item::Distinct(decl) => Some((decl.name, decl.name_span)),
                Item::TypeAlias(decl) => Some((decl.name, decl.name_span)),
                Item::Construct(decl) => Some((decl.name, decl.name_span)),
                _ => None,
            };
            let Some((name, span)) = named else {
                continue;
            };
            declared.push((source, name, span));
        }
        for (source, name, span) in declared {
            // Import visibility is a property of the declaring file, so the
            // "beyond my own package" resolvers are asked from that file's seat.
            self.source = source;
            let text = self.interner.resolve(name).to_owned();
            if !self.name_provided_by_import(&text) {
                continue;
            }
            self.emit(
                span,
                "KSEM003",
                format!(
                    "`{text}` is already provided by an imported module, so declaring it \
                     here shadows that type. Remove this declaration and use the imported \
                     `{text}`, or rename this one — a type name means exactly one declaration."
                ),
            );
        }
    }

    /// Whether a type of this name is visible from an imported module rather
    /// than this file's own package.
    ///
    /// One namespace, so a struct shadowing an imported enum is as much a
    /// collision as one shadowing an imported struct — both tables are asked.
    fn name_provided_by_import(&self, name: &str) -> bool {
        self.struct_beyond_own_package(name).is_some()
            || self.enum_beyond_own_package(name).is_some()
    }
}
