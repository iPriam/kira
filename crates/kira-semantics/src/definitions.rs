//! Reference→definition links, recorded while names resolve.
//!
//! Go-to-definition is served by the *same* resolution the compiler performs
//! — the analyzer records where each written name landed as it resolves it —
//! so an editor jump and the type checker can never disagree about what a
//! name means. The links are collected into [`crate::Analysis::definitions`]
//! and surface through [`crate::DefinitionAccumulator`].
//!
//! Declaration name spans are not in the semantic model (`StructDef`,
//! `EnumDef`, and friends carry names only), so [`DeclSpans`] indexes them out
//! of the syntax tree once, up front, and resolution sites look them up by
//! name — the same recovery `report_unknown_field_type` already performs one
//! diagnostic at a time.

use std::collections::HashMap;

use kira_core::Interner;
use kira_source::{FileSpan, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::Item;

use crate::analyze::Analyzer;

/// One resolved reference: where the name was written, where its definition is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DefinitionLink {
    /// The identifier token as written.
    pub reference: FileSpan,
    /// The name token of the declaration it resolves to.
    pub definition: FileSpan,
}

/// Declaration name spans, indexed by name out of the syntax tree.
///
/// Keys are resolved names: a type by its bare name, a field or variant by
/// `(owner, member)`. A name that misses here simply records no link — a jump
/// that goes nowhere, never one that goes somewhere wrong.
#[derive(Debug, Default)]
pub(crate) struct DeclSpans {
    /// Struct, class, enum (generic included), and alias declarations.
    types: HashMap<String, FileSpan>,
    /// Struct and class fields, keyed by `(owner name, field name)`.
    ///
    /// A field a class inherits is keyed under the ancestor that wrote it, so
    /// a read through the child records no link rather than a wrong one.
    fields: HashMap<(String, String), FileSpan>,
    /// Enum variants, keyed by `(enum name, variant name)`.
    variants: HashMap<(String, String), FileSpan>,
}

impl DeclSpans {
    /// Indexes every type, field, and variant declaration in the tree.
    pub(crate) fn collect(tree: &SyntaxTree, interner: &Interner) -> Self {
        let mut spans = Self::default();
        for (source, item) in tree.items_with_source() {
            match item {
                Item::Struct(declaration) => {
                    let owner = interner.resolve(declaration.name).to_owned();
                    spans
                        .types
                        .insert(owner.clone(), FileSpan::new(source, declaration.name_span));
                    for field in &declaration.fields {
                        spans.fields.insert(
                            (owner.clone(), interner.resolve(field.name).to_owned()),
                            FileSpan::new(source, field.name_span),
                        );
                    }
                }
                Item::Class(declaration) => {
                    let owner = interner.resolve(declaration.name).to_owned();
                    spans
                        .types
                        .insert(owner.clone(), FileSpan::new(source, declaration.name_span));
                    for field in &declaration.fields {
                        spans.fields.insert(
                            (owner.clone(), interner.resolve(field.name).to_owned()),
                            FileSpan::new(source, field.name_span),
                        );
                    }
                }
                Item::Enum(declaration) => {
                    let owner = interner.resolve(declaration.name).to_owned();
                    spans
                        .types
                        .insert(owner.clone(), FileSpan::new(source, declaration.name_span));
                    for variant in &declaration.variants {
                        spans.variants.insert(
                            (owner.clone(), interner.resolve(variant.name).to_owned()),
                            FileSpan::new(source, variant.name_span),
                        );
                    }
                }
                Item::TypeAlias(declaration) => {
                    spans.types.insert(
                        interner.resolve(declaration.name).to_owned(),
                        FileSpan::new(source, declaration.name_span),
                    );
                }
                Item::Function(_) | Item::Import(_) | Item::Unsupported(_) => {}
            }
        }
        spans
    }
}

impl Analyzer<'_> {
    /// Records that the name written at `reference` (in the file being
    /// analyzed) resolved to the declaration at `definition`.
    pub(crate) fn link(&mut self, reference: Span, definition: FileSpan) {
        self.definitions.push(DefinitionLink {
            reference: FileSpan::new(self.source, reference),
            definition,
        });
    }

    /// Links a written type name to its declaration, when one was declared.
    ///
    /// Builtins and bound type parameters have no declaration to jump to and
    /// record nothing.
    pub(crate) fn link_type_name(&mut self, name: &str, reference: Span) {
        if let Some(&definition) = self.decl_spans.types.get(name) {
            self.link(reference, definition);
        }
    }

    /// Links a written field name to its declaration on `owner`.
    pub(crate) fn link_field_name(&mut self, owner: &str, field: &str, reference: Span) {
        if let Some(&definition) = self
            .decl_spans
            .fields
            .get(&(owner.to_owned(), field.to_owned()))
        {
            self.link(reference, definition);
        }
    }

    /// Links a written variant name to its declaration on `owner`.
    pub(crate) fn link_variant_name(&mut self, owner: &str, variant: &str, reference: Span) {
        if let Some(&definition) = self
            .decl_spans
            .variants
            .get(&(owner.to_owned(), variant.to_owned()))
        {
            self.link(reference, definition);
        }
    }
}
