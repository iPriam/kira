//! The struct-attached `@FFI.*` type family: how each of the five forms is
//! realized, and the executable slice among them.
//!
//! Two of the forms become **type aliases** and never mint a struct id —
//! `@FFI.Alias` to its written `target`, `@FFI.Pointer` to [`Type::RawPtr`] —
//! and are registered in [`crate::aliases`] alongside `type Name = Target`. The
//! other three mint a nominal struct id, recorded here by [`FfiStructKind`]:
//!
//! * `@FFI.Struct { layout: c }` is a real C-layout struct. Its one runtime
//!   behavior is **zero-filled construction**: `StructType { ... }` and `StructType()` start from
//!   a zeroed value and apply the explicit initializers over it, exactly the
//!   oracle's construction rule. That lowers to an ordinary `StructNew` of zero
//!   literals, so every backend agrees with no new opcode — the zero-fill is a
//!   frontend rule, and the field types it can zero are the ones with a Kira
//!   literal ([`Type::Int`]/[`Type::Float`]/[`Type::Bool`]) plus nested C-layout
//!   aggregates. A field with no such zero (a `RawPtr`, a `CString`, an enum, a
//!   heap array) is refused precisely rather than mis-initialized.
//! * `@FFI.Array` and `@FFI.Callback` are declared as nominal types so the
//!   binding files that name them type-check, but their runtime behavior —
//!   inline element storage for the array, a native function pointer for the
//!   callback — is not yet executable, so any *use* is refused with a precise,
//!   typed "not yet executable" diagnostic rather than mis-lowered.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{FfiTypeKind, StructDecl};

use crate::analyze::Analyzer;

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

impl Analyzer<'_> {
    /// The struct id of `name` when it is a `@FFI.Struct { layout: c }` type,
    /// for the `StructType()` construction path.
    pub(crate) fn ffi_c_layout_named(&self, name: &str) -> Option<StructId> {
        let id = self.program.types.structs().lookup(name)?;
        (self.ffi_structs.get(&id) == Some(&FfiStructKind::CLayout)).then_some(id)
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

    /// The zero value for one field of a C-layout struct, or an error node with
    /// a precise refusal when the field's type cannot be zero-filled yet.
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
                         `{type_name}`, which has no zero value yet: give it an explicit \
                         initializer in the `{struct_name} {{ ... }}` literal"
                    ),
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
        }
    }

    /// A zero-filled HIR value of `ty`, or `None` when `ty` has no defined zero
    /// in this slice.
    ///
    /// The scalars zero to their Kira literal; a nested C-layout aggregate (a
    /// `@FFI.Struct`, or an empty `@FFI.Array`/`@FFI.Callback` nominal) zeroes
    /// field by field. A `RawPtr`, `CString`, `String`, enum, or heap array has
    /// no literal zero here and yields `None`.
    fn ffi_zero_value(&mut self, ty: Type, span: Span) -> Option<HirExprId> {
        let expr = match ty {
            Type::Int(_) => HirExpr::Int(0),
            Type::Float(_) => HirExpr::Float(0.0),
            Type::Bool => HirExpr::Bool(false),
            Type::Struct(id) if self.ffi_structs.contains_key(&id) => {
                return Some(self.ffi_zero_filled_struct(id, span));
            }
            _ => return None,
        };
        Some(self.program.exprs.alloc(expr))
    }

    /// Refuses a use of a `@FFI.Array`/`@FFI.Callback` type whose runtime
    /// behavior is not yet executable, with a message precise to the form.
    /// Returns an error node.
    pub(crate) fn refuse_ffi_not_executable(
        &mut self,
        kind: FfiStructKind,
        id: StructId,
        span: Span,
    ) -> HirExprId {
        self.emit_ffi_not_executable(kind, id, span);
        self.program.exprs.alloc(HirExpr::Error)
    }

    /// Emits the "not yet executable" refusal for a `@FFI.Array`/`@FFI.Callback`
    /// use, without minting an error node — for a position (a foreign seam type)
    /// that reports through its own `None`.
    pub(crate) fn emit_ffi_not_executable(
        &mut self,
        kind: FfiStructKind,
        id: StructId,
        span: Span,
    ) {
        let name = self.program.types.type_name(Type::Struct(id));
        let detail = match kind {
            FfiStructKind::Array => {
                "an inline fixed-size C array; indexing and element storage are declared but \
                 not yet executable"
            }
            FfiStructKind::Callback => {
                "a native function-pointer typedef; passing a Kira function across the C ABI is \
                 declared but not yet executable"
            }
            FfiStructKind::CLayout => "a C-layout struct",
        };
        self.emit(
            span,
            "KSEM187",
            format!(
                "`{name}` is `@{}` — {detail}. Its declaration type-checks, but this use is \
                 not yet supported.",
                kind.annotation()
            ),
        );
    }
}
