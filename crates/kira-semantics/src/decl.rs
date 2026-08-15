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
//! Two-phase collection permits forward references, so a struct can reach itself
//! through by-value fields. Such a cycle has no finite size.
//! [`Analyzer::break_struct_value_cycles`] finds each cycle, breaks its closing
//! field to `Error`, and reports it, which keeps
//! [`kira_semantics_model::TypeTable::owns_heap`] total without a visited set.
//!
//! The escape that diagnostic recommends is an enum, and a type may reach itself
//! through one, so a walk that builds a *value* rather than measuring a size
//! still needs a visited set. `Analyzer::check_enum_terminates` reports the enum
//! that leaves it no way out.

use std::collections::HashMap;

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{FieldDef, StructDef, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{Item, StructDecl};

use crate::analyze::{Analyzer, FieldDefault, FnCtx};
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
        // Last, because a `@FFI.Callback` may name a C-layout struct among its
        // parameters and the row describing that struct is built out of the
        // struct's fields — which are only in the table once every struct above
        // has been committed.
        self.resolve_callback_signatures(&headers);
    }

    /// Whether `name` is already an enum, reporting the collision when it is.
    ///
    /// Kira has ONE type namespace, and enums are declared before structs and
    /// classes — so a name an enum took is visible by the time either of those
    /// claims it. Letting both exist does not make the program ambiguous to the
    /// compiler, which consults one table first and finds a winner; it makes it
    /// ambiguous to the *reader*, and it moves the error to the use site. The
    /// enum's own uses then fail in ways that describe the struct that won:
    /// `match` on one reports "a `match` subject must be an enum", and naming a
    /// variant reports that the type is a class. Neither mentions the second
    /// declaration, which is the only thing that was actually wrong.
    pub(crate) fn name_taken_by_enum(&mut self, name: &str, span: Span, kind: &str) -> bool {
        if self.program.types.enums().lookup(name).is_none() {
            return false;
        }
        self.emit(
            span,
            "KSEM004",
            format!(
                "`{name}` is already defined as an enum, so this {kind} cannot take the \
                 same name: a type name means exactly one declaration"
            ),
        );
        true
    }

    /// First pass: declares every struct's name as an empty header, minting its
    /// id and reserving its `struct_defaults` slot.
    ///
    /// A duplicate name keeps the first declaration and is reported here, so the
    /// second pass never resolves fields for a name that lost the collision.
    fn declare_struct_headers(&mut self) -> Vec<(StructId, &'a StructDecl, SourceId)> {
        let tree: &'a SyntaxTree = self.tree;
        let mut headers = Vec::new();
        // What each accepted name already describes, so a repeat can be told
        // from a redescription. Keyed the way the table is — by owner package
        // and name — because that is the scope a name is unique in.
        let mut described: HashMap<(Option<String>, String), String> = HashMap::new();
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
            // The diagnostic below renders in the file the declaration was
            // written in, not in whichever file this pass happened to visit
            // last.
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            if self.name_taken_by_enum(&name, declaration.name_span, "struct") {
                continue;
            }
            // Filed under the package that wrote it, so two packages may each
            // declare the name and only a repeat *within* one is a duplicate.
            let owner = self.imports.package_of(source).map(str::to_owned);
            match self.program.types.structs_mut().declare_owned(
                owner.as_deref(),
                StructDef {
                    name: name.clone(),
                    fields: Vec::new(),
                },
            ) {
                Some(id) => {
                    // A `@FFI.Struct`/`Array`/`Callback` mints a nominal id; the
                    // kind decides zero-fill construction and use-site refusals.
                    if let Some(crate::ffi_types::FfiClassification::Struct(kind)) =
                        crate::ffi_types::classify(declaration)
                    {
                        self.ffi_structs.insert(id, kind);
                    }
                    self.struct_sources.insert(id, source);
                    // Reserve the defaults slot now, in id order, so a function
                    // type minted while the second pass resolves fields (which
                    // pushes its own slot) cannot land on this struct's id.
                    self.struct_defaults.push(Vec::new());
                    if let Some(description) = self.foreign_description(declaration) {
                        described.insert((owner, name), description);
                    }
                    headers.push((id, declaration, source));
                }
                // The name is taken in this package. A *foreign* declaration
                // that describes exactly what the accepted one describes is the
                // same C type arriving twice — autobind writes it into every
                // binding whose header uses it — so it is idempotent rather than
                // a collision. Anything else is one name given two meanings.
                None => {
                    let repeat = self
                        .foreign_description(declaration)
                        .is_some_and(|description| {
                            described.get(&(owner, name.clone())) == Some(&description)
                        });
                    if !repeat {
                        self.emit(
                            declaration.name_span,
                            "KSEM004",
                            format!("struct `{name}` is already defined"),
                        );
                    }
                }
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
            let (mut def, mut defaults) = self.resolve_struct_def(declaration);
            // An `@FFI.Array` declares its storage in the annotation rather than
            // the body, so its one field is synthesized here — after the body's
            // own fields, which the same pass refuses.
            if let Some(count) = self.ffi_array_storage(declaration, &mut def) {
                defaults.push(None);
                self.ffi_array_counts.insert(id, count);
            }
            // An `@FFI.Callback` likewise: its value is the C function pointer,
            // and the annotation says what that pointer's signature is.
            if self.ffi_callback_storage(declaration, &mut def) {
                defaults.push(None);
            }
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

    /// Breaks every by-value cycle left in the finished struct table.
    ///
    /// [`Self::break_struct_value_cycles`] sees only the structs of one pass, so
    /// it cannot see an edge through a **class** — and since a struct field may
    /// name a class and a class field may name a struct, a cycle can be spelled
    /// with either kind or both. This runs once the table is complete, which is
    /// the only point every edge exists, and reports what it breaks by the same
    /// rule and with the same code.
    ///
    /// A field broken to `Error` stays broken: the walk removes back-edges, so
    /// what is left can be recursed without a visited set — which is what every
    /// later pass over a field type relies on.
    pub(crate) fn break_remaining_value_cycles(&mut self) {
        let ids: Vec<StructId> = self.program.types.structs().ids().collect();
        let count = ids.len();
        let edges: Vec<Vec<(usize, usize)>> = ids
            .iter()
            .map(|&id| match self.program.types.structs().get(id) {
                Some(def) => def
                    .fields
                    .iter()
                    .enumerate()
                    .filter_map(|(slot, field)| match field.ty {
                        Type::Struct(target) => Some((slot, target.index() as usize)),
                        _ => None,
                    })
                    .collect(),
                None => Vec::new(),
            })
            .collect();

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
                let (slot, target) = edges[node][cursor];
                match colour.get(target).copied() {
                    Some(0) => {
                        colour[target] = 1;
                        stack.push((target, 0));
                    }
                    Some(1) => broken.push((node, slot)),
                    _ => {}
                }
            }
        }

        for (index, slot) in broken {
            let Some(&id) = ids.get(index) else {
                continue;
            };
            let name = self.program.types.type_name(Type::Struct(id));
            let mut fields = match self.program.types.structs().get(id) {
                Some(def) => def.fields.clone(),
                None => continue,
            };
            if let Some(field) = fields.get_mut(slot) {
                field.ty = Type::Error;
            }
            self.program.types.structs_mut().set_fields(id, fields);
            if let Some(source) = self.struct_sources.get(&id).copied() {
                self.source = source;
            }
            let span = self.declaration_name_span(&name).unwrap_or(Span::new(0, 0));
            self.emit(
                span,
                "KSEM052",
                format!(
                    "`{name}` cannot contain itself by value: its fields form a cycle with no \
                     finite size. Hold one of them behind an array (`[{name}]`) or an enum to \
                     break it."
                ),
            );
        }
    }

    /// The span of the `struct` or `class` declaration written under `name`.
    ///
    /// Found from the syntax rather than kept beside the row, because only a
    /// diagnostic ever wants it and only for a declaration that went wrong.
    fn declaration_name_span(&self, name: &str) -> Option<Span> {
        self.tree.items().iter().find_map(|item| match item {
            Item::Struct(declaration) if self.interner.resolve(declaration.name) == name => {
                Some(declaration.name_span)
            }
            Item::Class(declaration) if self.interner.resolve(declaration.name) == name => {
                Some(declaration.name_span)
            }
            _ => None,
        })
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
        // An `@FFI.*` declaration's fields describe a **C** layout, not a Kira
        // value: `struct sg_desc { var window_title: CString }` mirrors a
        // `const char*` member. So `CString` resolves here exactly as it does in
        // an `@FFI.Extern` signature, and the rule that keeps it out of
        // Kira-owned storage is unweakened — a C-layout struct has no
        // constructible `CString` field, and that is refused where the value
        // would be minted rather than where the layout is described.
        let outer_foreign = self.in_foreign_signature;
        self.in_foreign_signature = declaration.ffi.is_some();
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
        self.in_foreign_signature = outer_foreign;
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
        // Every declared struct, by id: a name no longer identifies one row,
        // because two packages may each declare it.
        let ids: Vec<StructId> = self.program.types.structs().ids().collect();
        for id in ids {
            // Construct-backed defaults are instance initializers: their
            // names are resolved in declaration order at each construction
            // site, where earlier fields have real per-instance locals.
            if self.constructs.contains_key(&id) {
                continue;
            }
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
        if self.constructs.contains_key(&id) {
            return None;
        }
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
        let mut empty = FnCtx::new(Type::Void);
        let resolved = self.analyze_default_in(&mut empty, default.syntax, declared);
        self.source = previous_source;
        self.resolving_struct_defaults.remove(&key);
        // A default whose analysis allocated locals cannot be shared: the
        // LocalId values belong to the probe context, and nested constructs may
        // also have hoisted binding statements that only make sense at the use
        // site. Keep the eager pass for diagnostics, but re-analyze such a
        // default in the caller's context below.
        if empty.locals.is_empty()
            && let Some(slot) = self
                .struct_defaults
                .get_mut(id.index() as usize)
                .and_then(|defaults| defaults.get_mut(index as usize))
                .and_then(Option::as_mut)
        {
            slot.resolved = Some(resolved);
        }
        Some(resolved)
    }

    /// Resolves a field default for an actual construction site. Cached HIR is
    /// safe only when the eager pass proved it introduced no locals; nested
    /// construct values are rebuilt in this function's local arena.
    pub(crate) fn resolve_field_default_at(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        index: u32,
    ) -> Option<HirExprId> {
        if self.constructs.contains_key(&id) {
            return None;
        }
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
        let resolved = self.analyze_default_in(ctx, default.syntax, declared);
        self.source = previous_source;
        self.resolving_struct_defaults.remove(&key);
        Some(resolved)
    }
}
