//! Declaring the program's types: the struct table and its field defaults.
//!
//! This runs before any signature is resolved and before any body is checked,
//! because a parameter, a local, or a field may name a struct — so the table
//! has to exist first.
//!
//! # Two phases, one flat scope
//!
//! A package is one flat module scope: every top-level struct in any of its
//! files is visible bare to every sibling, with no import and regardless of the
//! order the files were loaded in. So collection runs in two passes. The first
//! declares every struct's *name* — an empty header that mints its id. The
//! second resolves each struct's field types, by which point every sibling name
//! already resolves, so a field may name a struct declared later in this file or
//! in any other file of the package.
//!
//! # Why a cycle still has to be caught
//!
//! Resolving in declaration order used to make a value cycle unrepresentable:
//! a field could only name a struct declared before it, so a struct could not
//! reach itself. Two-phase collection lifts that ordering, so the cycle has to
//! be caught outright instead — a struct that reaches itself through by-value
//! fields would have no finite size. [`Analyzer::break_struct_value_cycles`]
//! finds each such cycle, breaks its closing field to `Error`, and reports it,
//! which is what keeps [`kira_semantics_model::TypeTable::owns_heap`] and
//! `default_value` recursing without a visited set.

use std::collections::HashMap;

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Item, StructDecl};

use crate::analyze::{Analyzer, FieldDefault};
use crate::classes::OwnMethod;
use crate::types::{AggregateKind, NameContext};

/// One struct whose name has been declared and whose fields have been resolved,
/// waiting to be committed to the table.
struct ResolvedStruct {
    /// The id minted for this struct's header in the first pass.
    id: StructId,
    /// The struct's name, for the value-cycle diagnostic.
    name: String,
    /// Where the name was written, for that diagnostic's span.
    name_span: Span,
    /// The file the struct was declared in, so the diagnostic renders there.
    source: SourceId,
    /// The resolved fields, index-aligned with `defaults`.
    fields: Vec<FieldDef>,
    /// The per-field defaults, as written, index-aligned with `fields`.
    defaults: Vec<Option<FieldDefault>>,
    /// The methods this struct declares itself, recorded for inheritance.
    methods: Vec<OwnMethod>,
}

impl<'a> Analyzer<'a> {
    /// Declares every struct in two passes: first every name, then every field.
    ///
    /// The first pass mints an id for each struct's name, so the second may
    /// resolve a field that names a sibling declared later — the flat-package
    /// scope makes declaration order irrelevant to visibility. A struct that
    /// reaches itself through by-value fields is then broken and reported,
    /// because such a value type has no finite size.
    pub(crate) fn collect_structs(&mut self) {
        let headers = self.declare_struct_headers();
        let resolved = self.resolve_struct_fields(&headers);
        let resolved = self.break_struct_value_cycles(resolved);
        for entry in resolved {
            self.commit_struct(entry);
        }
    }

    /// First pass: declares every struct's name as an empty header, minting its
    /// id and reserving its `struct_defaults` slot.
    ///
    /// A duplicate name keeps the first declaration and is reported here, so the
    /// second pass never resolves fields for a name that lost the collision.
    fn declare_struct_headers(&mut self) -> Vec<(StructId, &'a StructDecl, SourceId)> {
        let tree: &'a SyntaxTree = self.tree;
        let mut headers = Vec::new();
        for (source, item) in tree.items_with_source() {
            let Item::Struct(declaration) = item else {
                continue;
            };
            // `@FFI.Alias`/`@FFI.Pointer` become type aliases, not struct rows —
            // `collect_type_aliases` already registered them, so they take no
            // struct id here.
            if crate::ffi_types::is_alias_shaped(declaration) {
                continue;
            }
            let name = self.interner.resolve(declaration.name).to_owned();
            match self.program.types.structs_mut().declare(StructDef {
                name: name.clone(),
                fields: Vec::new(),
            }) {
                Some(id) => {
                    // A `@FFI.Struct`/`Array`/`Callback` mints a nominal id; the
                    // kind decides zero-fill construction and use-site refusals.
                    if let Some(crate::ffi_types::FfiClassification::Struct(kind)) =
                        crate::ffi_types::classify(declaration)
                    {
                        self.ffi_structs.insert(id, kind);
                    }
                    // Reserve the defaults slot now, in id order, so a function
                    // type minted while the second pass resolves fields (which
                    // pushes its own slot) cannot land on this struct's id.
                    self.struct_defaults.push(Vec::new());
                    headers.push((id, declaration, source));
                }
                None => self.emit(
                    declaration.name_span,
                    "KSEM004",
                    format!("struct `{name}` is already defined"),
                ),
            }
        }
        headers
    }

    /// Second pass: resolves each declared struct's fields, now that every
    /// struct name resolves.
    fn resolve_struct_fields(
        &mut self,
        headers: &[(StructId, &'a StructDecl, SourceId)],
    ) -> Vec<ResolvedStruct> {
        let mut resolved = Vec::with_capacity(headers.len());
        for &(id, declaration, source) in headers {
            // Field types resolve against the imports of the declaring file.
            self.source = source;
            let (def, defaults) = self.resolve_struct_def(declaration);
            // A class may extend a struct, so a struct's methods have to be
            // inheritable — which means recording them the same way a class's
            // are.
            let methods = declaration
                .methods
                .iter()
                .map(|method| OwnMethod {
                    name: self.interner.resolve(method.name).to_owned(),
                    arity: method.params.len(),
                })
                .collect();
            resolved.push(ResolvedStruct {
                id,
                name: def.name,
                name_span: declaration.name_span,
                source,
                fields: def.fields,
                defaults,
                methods,
            });
        }
        resolved
    }

    /// Breaks every by-value cycle among the resolved structs, reporting each.
    ///
    /// A field of struct type is stored inline, so `A` holding `B` while `B`
    /// holds `A` describes a value with no finite size. An array or enum field
    /// is a heap handle, so it is indirection that breaks a would-be cycle and
    /// is not an edge here. A three-colour depth-first walk finds each cycle at
    /// the back-edge that closes it, breaks that field to `Error`, and reports
    /// it — removing the back-edges of a walk leaves a graph with none, so what
    /// commits to the table can be recursed without a visited set.
    fn break_struct_value_cycles(
        &mut self,
        mut resolved: Vec<ResolvedStruct>,
    ) -> Vec<ResolvedStruct> {
        let index_of: HashMap<u32, usize> = resolved
            .iter()
            .enumerate()
            .map(|(position, entry)| (entry.id.index(), position))
            .collect();
        // The by-value edges of each struct: (field index, target position).
        let edges: Vec<Vec<(usize, usize)>> = resolved
            .iter()
            .map(|entry| {
                entry
                    .fields
                    .iter()
                    .enumerate()
                    .filter_map(|(field_index, field)| match field.ty {
                        Type::Struct(target) => index_of
                            .get(&target.index())
                            .map(|&position| (field_index, position)),
                        _ => None,
                    })
                    .collect()
            })
            .collect();
        let count = resolved.len();
        // 0 unvisited, 1 on the current path, 2 finished.
        let mut colour = vec![0u8; count];
        let mut broken: Vec<(usize, usize)> = Vec::new();
        for root in 0..count {
            if colour[root] != 0 {
                continue;
            }
            colour[root] = 1;
            let mut stack = vec![(root, 0usize)];
            while let Some((node, cursor)) = stack.pop() {
                if cursor == edges[node].len() {
                    colour[node] = 2;
                    continue;
                }
                stack.push((node, cursor + 1));
                let (field_index, target) = edges[node][cursor];
                match colour[target] {
                    0 => {
                        colour[target] = 1;
                        stack.push((target, 0));
                    }
                    1 => broken.push((node, field_index)),
                    _ => {}
                }
            }
        }
        for (node, field_index) in broken {
            if let Some(field) = resolved[node].fields.get_mut(field_index) {
                field.ty = Type::Error;
            }
            let ResolvedStruct {
                name,
                name_span,
                source,
                ..
            } = &resolved[node];
            let (name, name_span, source) = (name.clone(), *name_span, *source);
            self.source = source;
            self.emit(
                name_span,
                "KSEM052",
                format!(
                    "struct `{name}` cannot contain itself by value: its fields form a cycle \
                     with no finite size. Hold one of them behind an array (`[{name}]`) or an \
                     enum to break it."
                ),
            );
        }
        resolved
    }

    /// Commits one resolved struct: writes its fields into the reserved row and
    /// records its defaults and methods against the same id.
    fn commit_struct(&mut self, entry: ResolvedStruct) {
        let ResolvedStruct {
            id,
            fields,
            defaults,
            methods,
            ..
        } = entry;
        self.program.types.structs_mut().set_fields(id, fields);
        if let Some(slot) = self.struct_defaults.get_mut(id.index() as usize) {
            *slot = defaults;
        }
        self.own_methods.insert(id, methods);
    }

    /// Resolves one struct declaration's fields against every declared struct,
    /// reporting duplicate fields and keeping the first of each.
    ///
    /// Returns the definition and its per-field defaults, index-aligned: a
    /// field dropped as a duplicate is dropped from both.
    fn resolve_struct_def(
        &mut self,
        declaration: &StructDecl,
    ) -> (StructDef, Vec<Option<FieldDefault>>) {
        let name = self.interner.resolve(declaration.name).to_owned();
        let mut fields: Vec<FieldDef> = Vec::with_capacity(declaration.fields.len());
        let mut defaults: Vec<Option<FieldDefault>> = Vec::with_capacity(declaration.fields.len());
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
                owner_kind: AggregateKind::Struct,
                owner: name.clone(),
            };
            let ty = self.resolve_type_in(field.ty, &context);
            fields.push(FieldDef {
                name: field_name,
                ty,
                mutable: field.mutable,
            });
            defaults.push(
                field
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source)),
            );
        }
        (StructDef { name, fields }, defaults)
    }

    /// The default initializer recorded for field `index` of `id`, if any.
    pub(crate) fn field_default(&self, id: StructId, index: u32) -> Option<FieldDefault> {
        self.struct_defaults
            .get(id.index() as usize)
            .and_then(|defaults| defaults.get(index as usize))
            .copied()
            .flatten()
    }

    /// Resolves every declared field default once, in its declaring file.
    pub(crate) fn resolve_struct_defaults(&mut self) {
        let ids: Vec<StructId> = self
            .program
            .types
            .structs()
            .defs()
            .iter()
            .filter_map(|def| self.program.types.structs().lookup(&def.name))
            .collect();
        for id in ids {
            let field_count = self
                .struct_defaults
                .get(id.index() as usize)
                .map_or(0, Vec::len);
            for field_index in 0..field_count as u32 {
                self.resolve_field_default(id, field_index);
            }
        }
    }

    /// Returns one resolved field default, resolving it in declaration scope on
    /// first use when recursive aggregate defaults reach it before the outer pass.
    pub(crate) fn resolve_field_default(&mut self, id: StructId, index: u32) -> Option<HirExprId> {
        let default = self.field_default(id, index)?;
        if let Some(resolved) = default.resolved {
            return Some(resolved);
        }

        let key = (id.index(), index);
        if !self.resolving_struct_defaults.insert(key) {
            let previous_source = self.source;
            self.source = default.source;
            self.emit(
                self.tree.expr(default.syntax).span(),
                "KSEM213",
                "field defaults recursively construct each other and have no finite value",
            );
            self.source = previous_source;
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }

        let declared = self
            .program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.field(index))
            .map(|field| field.ty);
        let previous_source = self.source;
        self.source = default.source;
        let resolved = self.analyze_default(default.syntax, declared);
        self.source = previous_source;
        self.resolving_struct_defaults.remove(&key);
        if let Some(slot) = self
            .struct_defaults
            .get_mut(id.index() as usize)
            .and_then(|defaults| defaults.get_mut(index as usize))
            .and_then(Option::as_mut)
        {
            slot.resolved = Some(resolved);
        }
        Some(resolved)
    }
}
