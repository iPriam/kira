//! The struct-attached `@FFI.*` type family and its C representations.
//!
//! Two of the forms become **type aliases** and never mint a struct id —
//! `@FFI.Alias` to its written `target`, `@FFI.Pointer` to [`Type::RawPtr`] —
//! and are registered in [`crate::aliases`] alongside `type Name = Target`. The
//! other three mint a nominal struct id, recorded here by [`FfiStructKind`]:
//!
//! * `@FFI.Struct { layout: c }` is a real C-layout struct. Construction starts
//!   from zero and applies explicit initializers. Scalars, pointer words, and
//!   nested foreign aggregates have defined zero values; a field without one is
//!   refused rather than mis-initialized.
//! * `@FFI.Array` is a nominal fixed-size C array. Its `elements` field has the
//!   declared extent and can appear in C-layout aggregates or fill a pointer to
//!   its elements. A standalone C parameter or result is refused because C
//!   decays an array to a pointer.
//! * `@FFI.Callback` is a nominal function-pointer type with a `RawPtr` field.
//!   A matching Kira function records a callback entry and receives a generated
//!   C thunk; unsupported signature positions are refused at the use site.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{FfiTypeKind, StructDecl};

use crate::analyze::Analyzer;
use crate::types::{AggregateKind, NameContext};

/// Which of the three struct-minting `@FFI.*` forms a struct id came from.
///
/// `@FFI.Alias` and `@FFI.Pointer` are absent by construction: they become
/// aliases and never reach the struct table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FfiStructKind {
    /// `@FFI.Struct { layout: c }` — a C-layout struct, zero-filled on
    /// construction.
    CLayout,
    /// `@FFI.Array { element; count }` — an inline fixed-size C array typedef.
    Array,
    /// `@FFI.Callback { params; result }` — a function-pointer typedef.
    Callback,
}

impl FfiStructKind {
    /// The annotation name this kind came from, for diagnostics.
    fn annotation(self) -> &'static str {
        match self {
            FfiStructKind::CLayout => "FFI.Struct",
            FfiStructKind::Array => "FFI.Array",
            FfiStructKind::Callback => "FFI.Callback",
        }
    }
}

/// How a `@FFI.*` struct annotation is realized: an alias, or a struct id of a
/// given kind.
#[derive(Debug, Clone, Copy)]
pub(crate) enum FfiClassification {
    /// `@FFI.Alias` — a plain typedef to its `target`.
    Alias,
    /// `@FFI.Pointer` — a pointer alias to [`Type::RawPtr`].
    Pointer,
    /// `@FFI.Struct`/`Array`/`Callback` — a nominal struct id of this kind.
    Struct(FfiStructKind),
}

/// Classifies a struct declaration by the `@FFI.*` annotation it carries, or
/// `None` when it carries none (a plain Kira struct).
pub(crate) fn classify(decl: &StructDecl) -> Option<FfiClassification> {
    let mark = decl.ffi.as_ref()?;
    Some(match &mark.kind {
        FfiTypeKind::Alias { .. } => FfiClassification::Alias,
        FfiTypeKind::Pointer { .. } => FfiClassification::Pointer,
        FfiTypeKind::Struct { .. } => FfiClassification::Struct(FfiStructKind::CLayout),
        FfiTypeKind::Array { .. } => FfiClassification::Struct(FfiStructKind::Array),
        FfiTypeKind::Callback { .. } => FfiClassification::Struct(FfiStructKind::Callback),
    })
}

/// Whether a declaration becomes a type alias (`@FFI.Alias`/`@FFI.Pointer`)
/// rather than a struct id, so the struct-table and struct-name-collision passes
/// skip it.
pub(crate) fn is_alias_shaped(decl: &StructDecl) -> bool {
    matches!(
        classify(decl),
        Some(FfiClassification::Alias) | Some(FfiClassification::Pointer)
    )
}

/// The name of the one field an `@FFI.Array` type carries its elements in.
///
/// A C array typedef has no members of its own, so the storage needs a name to
/// be reachable from Kira at all — and naming it rather than hiding it means
/// `handle.elements[3]` is ordinary array indexing with no new machinery.
pub(crate) const FFI_ARRAY_FIELD: &str = "elements";

/// The name of the one field an `@FFI.Callback` type carries its C function
/// pointer in.
pub(crate) const FFI_CALLBACK_FIELD: &str = "pointer";

/// Whether a foreign declaration is a **forward declaration**: it names a C type
/// without describing it.
///
/// The C `typedef union X X;` / `typedef struct _GUID GUID;` idiom, which a
/// header emits ahead of the real definition. Autobind carries it across as an
/// alias-shaped declaration with an empty body, and the definition that
/// completes it arrives later in the same file.
pub(crate) fn is_forward_declaration(decl: &StructDecl) -> bool {
    is_alias_shaped(decl) && decl.fields.is_empty() && decl.methods.is_empty()
}

/// Whether a foreign declaration **describes** the type it names, rather than
/// only naming it.
pub(crate) fn is_foreign_definition(decl: &StructDecl) -> bool {
    matches!(
        decl.ffi.as_ref().map(|mark| &mark.kind),
        Some(FfiTypeKind::Struct { .. })
    ) && !decl.fields.is_empty()
}

impl Analyzer<'_> {
    /// A foreign declaration's description of the C type it names, as written,
    /// or `None` for a declaration that is not foreign at all.
    ///
    /// Autobind emits a description of a C type into every binding that uses it,
    /// so the same type arrives more than once: `@FFI.Pointer { target: char;
    /// ownership: borrowed; } struct char_ptr {}` is written in both
    /// `sokol.kira` and `vulkan.kira`, and `struct U32_array_2 {}` in every
    /// binding whose header has a `uint32_t[2]`. Two declarations of one name
    /// with equal descriptions describe **one** C type, so the repeat is
    /// idempotent rather than a collision.
    ///
    /// Two with *different* descriptions still collide: that is one name given
    /// two meanings, which is the thing the duplicate rule exists to catch.
    pub(crate) fn foreign_description(&self, decl: &StructDecl) -> Option<String> {
        let kind = &decl.ffi.as_ref()?.kind;
        let word = |value: &Option<(kira_core::Symbol, Span)>| match value {
            Some((symbol, _)) => self.interner.resolve(*symbol).to_owned(),
            None => "?".to_owned(),
        };
        let ty = |target: &Option<kira_syntax_model::ast::TypeRefId>| match target {
            Some(id) => self.written_type_name(*id),
            None => "?".to_owned(),
        };
        let head = match kind {
            FfiTypeKind::Alias { target } => format!("alias({})", ty(target)),
            FfiTypeKind::Pointer { target, ownership } => {
                format!("pointer({},{})", ty(target), word(ownership))
            }
            FfiTypeKind::Array { element, count } => format!(
                "array({},{})",
                ty(element),
                count.map_or("?".to_owned(), |(count, _)| count.to_string())
            ),
            FfiTypeKind::Callback {
                abi,
                params,
                result,
            } => {
                let params: Vec<String> = params
                    .iter()
                    .map(|&param| self.written_type_name(param))
                    .collect();
                format!(
                    "callback({},[{}],{})",
                    word(abi),
                    params.join(","),
                    ty(result)
                )
            }
            FfiTypeKind::Struct { layout } => format!("struct({})", word(layout)),
        };
        let fields: Vec<String> = decl
            .fields
            .iter()
            .map(|field| {
                format!(
                    "{}:{}",
                    self.interner.resolve(field.name),
                    self.written_type_name(field.ty)
                )
            })
            .collect();
        Some(format!("{head}{{{}}}", fields.join(",")))
    }

    /// A written type as it was spelled, for comparing two declarations without
    /// resolving either — the tables they would resolve against are not built
    /// when the comparison is needed.
    pub(crate) fn written_type_name(&self, id: kira_syntax_model::ast::TypeRefId) -> String {
        use kira_syntax_model::ast::TypeRef;
        match self.tree.type_ref(id) {
            TypeRef::Named { name, .. } => self.interner.resolve(*name).to_owned(),
            TypeRef::Array { element, .. } => format!("[{}]", self.written_type_name(*element)),
            TypeRef::Generic { name, args, .. } => {
                let args: Vec<String> = args
                    .iter()
                    .map(|&arg| self.written_type_name(arg))
                    .collect();
                format!("{}<{}>", self.interner.resolve(*name), args.join(","))
            }
            TypeRef::Function { params, result, .. } => {
                let params: Vec<String> = params
                    .iter()
                    .map(|&param| self.written_type_name(param))
                    .collect();
                format!(
                    "({})->{}",
                    params.join(","),
                    self.written_type_name(*result)
                )
            }
            TypeRef::SomeConstruct { family, .. } => {
                format!("some {}", self.interner.resolve(*family))
            }
            // Spelled by its two halves rather than by what it resolves to,
            // because this compares declarations *before* the tables that would
            // resolve it exist. Two shorthands for the same member of the same
            // family describe the same thing; two for different members do not.
            TypeRef::ConstructMember { family, member, .. } => format!(
                "{}::{}",
                self.interner.resolve(*family),
                self.interner.resolve(*member)
            ),
            // The parser already reported this; two unresolvable spellings are
            // never treated as the same description.
            TypeRef::Error { span } => format!("<error@{}>", span.start),
        }
    }
}

impl Analyzer<'_> {
    /// Gives an `@FFI.Array` declaration its element storage: one field holding
    /// a Kira array of the annotation's element type.
    ///
    /// Returns the C extent, which the seam needs and the Kira type does not
    /// carry — a Kira array's length is its own, while the C declaration
    /// reserves exactly `count` elements.
    pub(crate) fn ffi_array_storage(
        &mut self,
        declaration: &StructDecl,
        def: &mut kira_semantics_model::StructDef,
    ) -> Option<u32> {
        let mark = declaration.ffi.as_ref()?;
        let FfiTypeKind::Array { element, count } = &mark.kind else {
            return None;
        };
        let name = self.interner.resolve(declaration.name).to_owned();
        if !def.fields.is_empty() {
            self.emit(
                mark.block_span,
                "KSEM243",
                format!(
                    "`{name}` is `@FFI.Array`, whose storage is the annotation's `element` and \
                     `count`: its body declares fields, which have no place in a C array"
                ),
            );
            return None;
        }
        let Some(element) = element else {
            self.emit(
                mark.block_span,
                "KSEM243",
                format!("`{name}` is `@FFI.Array` but names no `element` type"),
            );
            return None;
        };
        let context = NameContext::Field {
            owner_kind: AggregateKind::Struct,
            owner: name.clone(),
        };
        let element = self.resolve_type_in(*element, &context);
        let Some((count, count_span)) = *count else {
            self.emit(
                mark.block_span,
                "KSEM243",
                format!("`{name}` is `@FFI.Array` but names no element `count`"),
            );
            return None;
        };
        // C has no zero-length array, and a negative one is not a count at all.
        let extent = u32::try_from(count).ok().filter(|count| *count > 0);
        let Some(count) = extent else {
            self.emit(
                count_span,
                "KSEM243",
                format!("`{name}` declares {count} elements; a C array holds at least one"),
            );
            return None;
        };
        def.fields.push(kira_semantics_model::FieldDef {
            name: FFI_ARRAY_FIELD.to_owned(),
            ty: self.program.types.array_of(element),
            mutable: true,
        });
        Some(count)
    }

    /// The C extent of an `@FFI.Array` type, when `id` is one.
    pub(crate) fn ffi_array_count(&self, id: StructId) -> Option<u32> {
        self.ffi_array_counts.get(&id).copied()
    }

    /// What one element of an `@FFI.Array` type is, when `id` is one.
    ///
    /// Read back off the storage field the declaration was given rather than
    /// off the annotation, so it is the resolved type the rest of the analyzer
    /// compares against.
    pub(crate) fn ffi_array_element(&self, id: StructId) -> Option<Type> {
        self.ffi_array_count(id)?;
        let field = self.program.types.structs().get(id)?.fields.first()?;
        self.program.types.element_of(field.ty)
    }

    /// Gives an `@FFI.Callback` declaration its storage: one `RawPtr` field
    /// holding the C function pointer.
    ///
    /// A callback typedef *is* a pointer in C, and a Kira struct wrapping one
    /// pointer has a pointer's size, alignment, and ABI class — so the type
    /// stays nominal (its declared signature is what a Kira function is checked
    /// against) while its value is the address C calls.
    pub(crate) fn ffi_callback_storage(
        &mut self,
        declaration: &StructDecl,
        def: &mut kira_semantics_model::StructDef,
    ) -> bool {
        let Some(mark) = declaration.ffi.as_ref() else {
            return false;
        };
        if !matches!(mark.kind, FfiTypeKind::Callback { .. }) {
            return false;
        }
        if !def.fields.is_empty() {
            let name = self.interner.resolve(declaration.name).to_owned();
            self.emit(
                mark.block_span,
                "KSEM243",
                format!(
                    "`{name}` is `@FFI.Callback`, whose shape is the annotation's `params` and \
                     `result`: its body declares fields, which have no place in a function pointer"
                ),
            );
            return false;
        }
        // The signature itself is not resolved here. It may name a C-layout
        // struct, whose row is built out of that struct's fields — and this pass
        // is the one *building* fields, so the table it would read is still half
        // empty. [`Analyzer::resolve_callback_signatures`] runs once every
        // struct is committed.
        def.fields.push(kira_semantics_model::FieldDef {
            name: FFI_CALLBACK_FIELD.to_owned(),
            ty: Type::RawPtr,
            mutable: true,
        });
        true
    }

    /// Resolves every `@FFI.Callback` declaration's C signature, once the struct
    /// table is complete.
    ///
    /// Keyed by the id the header pass minted rather than by looking the name up
    /// again: the declaration and its id are the same row, and a lookup would
    /// have to reconstruct which package owns it.
    pub(crate) fn resolve_callback_signatures(
        &mut self,
        headers: &[(StructId, &StructDecl, kira_source::SourceId)],
    ) {
        for &(id, declaration, source) in headers {
            let Some(mark) = declaration.ffi.as_ref() else {
                continue;
            };
            let FfiTypeKind::Callback { params, result, .. } = &mark.kind else {
                continue;
            };
            let (params, result) = (params.clone(), *result);
            // The written types resolve against the imports of the file that
            // wrote them, not of whichever file this pass visited last.
            self.source = source;
            if let Some(signature) = self.resolve_callback_signature(&params, result) {
                self.ffi_callback_signatures.insert(id, signature);
            }
        }
    }

    /// The struct id of `name` when it is a `@FFI.*` type that constructs from
    /// a zeroed value: a C-layout struct, an inline array, or a callback.
    ///
    /// All three describe C storage whose zero is defined: a zeroed struct, an
    /// array with its declared extent, and a null function pointer. An omitted
    /// field takes that zero rather than being reported missing.
    pub(crate) fn ffi_c_layout_named(&self, name: &str) -> Option<StructId> {
        let id = self.visible_struct(name)?;
        self.ffi_structs.contains_key(&id).then_some(id)
    }

    /// The `@FFI.*` kind of a struct id, when it came from one.
    pub(crate) fn ffi_struct_kind(&self, id: StructId) -> Option<FfiStructKind> {
        self.ffi_structs.get(&id).copied()
    }

    /// Builds a fully zero-filled value of a C-layout struct — the `StructType()` form.
    ///
    /// Every field takes its zero; a field whose type has no defined zero in
    /// this slice is refused precisely and filled with an error node, so the
    /// construction still yields a `StructNew` of the right arity.
    pub(crate) fn ffi_zero_filled_struct(&mut self, id: StructId, span: Span) -> HirExprId {
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map_or(0, |def| def.fields.len());
        let mut fields = Vec::with_capacity(field_count);
        for index in 0..field_count as u32 {
            fields.push(self.ffi_zero_field(id, index, span));
        }
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }

    /// The zero value for one field of a C-layout struct, or an error node when
    /// the field's type has no defined C zero.
    pub(crate) fn ffi_zero_field(&mut self, id: StructId, index: u32, span: Span) -> HirExprId {
        let field = self
            .program
            .types
            .structs()
            .get(id)
            .and_then(|def| def.field(index))
            .map(|field| (field.name.clone(), field.ty));
        let Some((field_name, field_ty)) = field else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        // An `@FFI.Array` field has the annotation's C extent, so every
        // declared element has storage for indexed writes.
        if let Some(&extent) = self.ffi_array_counts.get(&id)
            && let Some(element) = self.program.types.element_of(field_ty)
        {
            let mut elements = Vec::with_capacity(extent as usize);
            for _ in 0..extent {
                elements.push(self.ffi_zero_field_value(element, field_ty, &field_name, id, span));
            }
            return self.program.exprs.alloc(HirExpr::ArrayNew {
                ty: field_ty,
                elements,
            });
        }
        match self.ffi_zero_value(field_ty, span) {
            Some(zero) => zero,
            None => {
                let struct_name = self.program.types.type_name(Type::Struct(id));
                let type_name = self.type_name(field_ty);
                self.emit(
                    span,
                    "KSEM186",
                    format!(
                        "field `{field_name}` of C-layout `{struct_name}` has type \
                         `{type_name}`, which has no defined zero value: give it an explicit \
                         initializer in the `{struct_name} {{ ... }}` literal"
                    ),
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
        }
    }

    /// One element's zero, reporting against the containing field.
    fn ffi_zero_field_value(
        &mut self,
        element: Type,
        field_ty: Type,
        field_name: &str,
        owner: StructId,
        span: Span,
    ) -> HirExprId {
        if let Some(zero) = self.ffi_zero_value(element, span) {
            return zero;
        }
        let struct_name = self.program.types.type_name(Type::Struct(owner));
        let type_name = self.type_name(field_ty);
        self.emit(
            span,
            "KSEM186",
            format!(
                "field `{field_name}` of C-layout `{struct_name}` has type \
                 `{type_name}`, which has no defined zero value: give it an explicit \
                 initializer in the `{struct_name} {{ ... }}` literal"
            ),
        );
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// A zero-filled HIR value of `ty`, or `None` when `ty` has no defined zero
    /// in this slice.
    ///
    /// The scalars zero to their Kira literal; a nested foreign aggregate
    /// zeroes field by field; every pointer word (`RawPtr`, `CString`, an
    /// `@FFI.Pointer`, or callback storage) zeroes to `NULL`. An ordinary
    /// `String`, enum, or heap array has no literal zero here and yields `None`.
    fn ffi_zero_value(&mut self, ty: Type, span: Span) -> Option<HirExprId> {
        let expr = match ty {
            Type::Int(_) => HirExpr::Int(0),
            Type::Float(_) => HirExpr::Float(0.0),
            Type::Bool => HirExpr::Bool(false),
            Type::Struct(id) if self.ffi_structs.contains_key(&id) => {
                return Some(self.ffi_zero_filled_struct(id, span));
            }
            // Every foreign pointer word, including callback storage, zeroes
            // to C's `NULL`.
            Type::RawPtr | Type::ForeignPtr(_) => HirExpr::RawPtrNull,
            Type::CString => HirExpr::CStringNull,
            // An ordinary Kira array has an empty zero value. `@FFI.Array`
            // fields are expanded to their declared extent by `ffi_zero_field`.
            Type::Array(_) => HirExpr::ArrayNew {
                ty,
                elements: Vec::new(),
            },
            _ => return None,
        };
        Some(self.program.exprs.alloc(expr))
    }

    /// Emits the refusal for an `@FFI.Array` used in a standalone foreign
    /// position, without minting an error node for the caller's `None`.
    pub(crate) fn emit_ffi_not_executable(
        &mut self,
        kind: FfiStructKind,
        id: StructId,
        span: Span,
    ) {
        let name = self.program.types.type_name(Type::Struct(id));
        let detail = match kind {
            // C keeps Array storage inline in a struct member but decays it to
            // a pointer in a parameter or result position.
            FfiStructKind::Array => {
                "an inline fixed-size C array. It crosses as a member of a `@FFI.Struct \
                 { layout: c }`; on its own, C decays an array to a pointer, so declare \
                 `RawPtr` when that is what the symbol takes"
            }
            FfiStructKind::Callback => {
                "a native function-pointer typedef, which uses a generated callback entry thunk"
            }
            FfiStructKind::CLayout => "a C-layout struct",
        };
        self.emit(
            span,
            "KSEM187",
            format!(
                "`{name}` is `@{}` — {detail}. This use is not supported in this position.",
                kind.annotation()
            ),
        );
    }
}
