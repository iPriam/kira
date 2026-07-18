//! Declaring the program's types: the struct table and its field defaults.
//!
//! This runs before any signature is resolved and before any body is checked,
//! because a parameter, a local, or a field may name a struct — so the table
//! has to exist first.

use kira_semantics_model::{FieldDef, StructDef};
use kira_syntax_model::ast::{ExprId, Item, StructDecl};

use crate::analyze::Analyzer;
use crate::types::NameContext;

impl Analyzer<'_> {
    /// Declares every struct, in source order, resolving field types as it goes.
    ///
    /// A field may only name a struct declared *earlier* in the file. That is
    /// not an arbitrary restriction: a struct is a value type, so a struct that
    /// could reach itself through its fields would have no finite size.
    /// Resolving in declaration order makes the cycle unrepresentable rather
    /// than something to detect after the fact — which is also what lets
    /// `StructTable::owns_heap` recurse without a visited set.
    pub(crate) fn collect_structs(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Struct(declaration) = item else {
                continue;
            };
            // Field types resolve against the imports of the declaring file.
            self.source = source;
            let (def, defaults) = self.resolve_struct_def(declaration);
            let name = def.name.clone();
            match self.program.types.structs_mut().declare(def) {
                // Pushed only on success, which is what keeps `struct_defaults`
                // indexed by the same ids the table mints.
                Some(_) => self.struct_defaults.push(defaults),
                None => self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("struct `{name}` is already defined"),
                ),
            }
        }
    }

    /// Resolves one struct declaration's fields against the structs declared so
    /// far, reporting duplicate fields and keeping the first of each.
    ///
    /// Returns the definition and its per-field defaults, index-aligned: a
    /// field dropped as a duplicate is dropped from both.
    fn resolve_struct_def(&mut self, declaration: &StructDecl) -> (StructDef, Vec<Option<ExprId>>) {
        let name = self.interner.resolve(declaration.name).to_owned();
        let mut fields: Vec<FieldDef> = Vec::with_capacity(declaration.fields.len());
        let mut defaults: Vec<Option<ExprId>> = Vec::with_capacity(declaration.fields.len());
        for field in &declaration.fields {
            let field_name = self.interner.resolve(field.name).to_owned();
            if fields.iter().any(|existing| existing.name == field_name) {
                self.emit(
                    field.name_span,
                    "KSEM005",
                    format!("struct `{name}` already has a field named `{field_name}`"),
                );
                continue;
            }
            let context = NameContext::Field {
                owner: name.clone(),
            };
            let ty = self.resolve_type_in(field.ty, &context);
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: field.mutable,
            });
            defaults.push(field.default);
        }
        (StructDef { name, fields }, defaults)
    }

    /// The default initializer written for field `index` of `id`, if any.
    pub(crate) fn field_default(
        &self,
        id: kira_semantics_model::StructId,
        index: u32,
    ) -> Option<ExprId> {
        self.struct_defaults
            .get(id.index() as usize)
            .and_then(|defaults| defaults.get(index as usize))
            .copied()
            .flatten()
    }
}
