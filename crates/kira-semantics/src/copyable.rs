//! `@Derive(Copy)`: the builtin copyability assertion.
//!
//! `Copy` is the one derive the compiler owns rather than a macro. It generates
//! no code and produces no free function — it is an *opt-in assertion* that a
//! type is structurally copyable, and its whole value is that the assertion is
//! checked.
//!
//! Kira already classifies copyability structurally and silently: a type is
//! copyable when every field or variant payload is, and it starts moving the
//! moment it gains a heap-owning field (a `String`, an array), an opaque
//! payload (a callback, native state), or anything that transitively contains
//! one. That flip happens with no signal at the declaration, which is exactly
//! what this makes visible. On an eligible type the derive is a no-op and
//! grants no new powers; on an ineligible one it is `KIR005`, naming the first
//! offending field or payload and its type.

use std::collections::HashSet;

use kira_semantics_model::{EnumId, StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::Item;

use crate::analyze::Analyzer;

/// The reason a type is not copyable: the member that owns something, and what
/// it owns.
struct NotCopyable {
    /// The field or variant that made the type move.
    member: String,
    /// That member's type, as a user sees it.
    ty: String,
    /// The type the member belongs to, when it is not the annotated one.
    ///
    /// `Some` for a transitively non-copyable member, so a nested struct that
    /// owns a `String` is reported at the member that owns it rather than at the
    /// field that merely holds it.
    owner: Option<String>,
}

impl Analyzer<'_> {
    /// Checks every `@Derive(Copy)` in the program.
    ///
    /// Runs after every struct, class, enum, and construct-backed type exists,
    /// because a field may name any of them and copyability is a question about
    /// the whole reachable shape.
    pub(crate) fn check_copy_derives(&mut self) {
        let claims: Vec<(SourceId, Span, String, Type)> = self.copy_claims();
        for (source, span, name, ty) in claims {
            self.source = source;
            let mut seen = HashSet::new();
            if let Some(reason) = self.not_copyable(ty, &mut seen) {
                let NotCopyable { member, ty, owner } = reason;
                let where_it_is = match owner {
                    Some(owner) if owner != name => format!("`{owner}`'s member `{member}`"),
                    _ => format!("its member `{member}`"),
                };
                self.emit(
                    span,
                    "KIR005",
                    format!(
                        "`{name}` derives `Copy`, but {where_it_is} has type `{ty}`, which is not \
                         copyable — it owns storage a copy would have to clone — so `{name}` moves \
                         rather than copies. Remove the derive and let it move, borrow it, or give \
                         it an explicit duplication."
                    ),
                );
            }
        }
    }

    /// Every `@Derive(Copy)` written in the program, resolved to its type.
    fn copy_claims(&self) -> Vec<(SourceId, Span, String, Type)> {
        let mut claims = Vec::new();
        for (source, item) in self.tree.items_with_source() {
            let (span, name, ty) = match item {
                Item::Struct(declaration) => {
                    let Some(span) = declaration.derives_copy else {
                        continue;
                    };
                    let name = self.interner.resolve(declaration.name).to_owned();
                    let Some(id) = self.program.types.structs().lookup(&name) else {
                        continue;
                    };
                    (span, name, Type::Struct(id))
                }
                Item::Enum(declaration) => {
                    let Some(span) = declaration.derives_copy else {
                        continue;
                    };
                    let name = self.interner.resolve(declaration.name).to_owned();
                    let Some(id) = self.program.types.enums().lookup(&name) else {
                        continue;
                    };
                    (span, name, Type::Enum(id))
                }
                _ => continue,
            };
            claims.push((source, span, name, ty));
        }
        claims
    }

    /// The first reason `ty` is not copyable, or `None` when it is.
    ///
    /// `seen` breaks a recursive type: a shape already being examined cannot be
    /// the thing that makes itself move, so it contributes nothing rather than
    /// looping.
    fn not_copyable(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<NotCopyable> {
        match ty {
            Type::Struct(id) => self.struct_not_copyable(id, seen),
            Type::Enum(id) => self.enum_not_copyable(id, seen),
            _ => None,
        }
    }

    /// The first field of `id` that is not copyable.
    fn struct_not_copyable(&self, id: StructId, seen: &mut HashSet<Type>) -> Option<NotCopyable> {
        if !seen.insert(Type::Struct(id)) {
            return None;
        }
        let def = self.program.types.structs().get(id)?;
        let owner = def.name.clone();
        for field in &def.fields {
            if let Some(reason) = self.member_not_copyable(&owner, &field.name, field.ty, seen) {
                return Some(reason);
            }
        }
        None
    }

    /// The first variant payload of `id` that is not copyable.
    fn enum_not_copyable(&self, id: EnumId, seen: &mut HashSet<Type>) -> Option<NotCopyable> {
        if !seen.insert(Type::Enum(id)) {
            return None;
        }
        let def = self.program.types.enums().get(id)?;
        let owner = def.name.clone();
        for variant in &def.variants {
            let Some(payload) = variant.payload else {
                continue;
            };
            if let Some(reason) = self.member_not_copyable(&owner, &variant.name, payload, seen) {
                return Some(reason);
            }
        }
        None
    }

    /// Whether one member makes its owner move, and why.
    fn member_not_copyable(
        &self,
        owner: &str,
        member: &str,
        ty: Type,
        seen: &mut HashSet<Type>,
    ) -> Option<NotCopyable> {
        if is_leaf_copyable(ty) {
            return None;
        }
        // A struct or an enum is copyable exactly when everything it holds is,
        // so the answer comes from inside it and the *inner* member is what the
        // diagnostic names — that is where the fix goes.
        if matches!(ty, Type::Struct(_) | Type::Enum(_)) {
            return self.not_copyable(ty, seen);
        }
        Some(NotCopyable {
            member: member.to_owned(),
            ty: self.type_name(ty),
            owner: Some(owner.to_owned()),
        })
    }
}

/// Whether `ty` is copyable without looking inside anything.
///
/// The scalar and pointer-word values, and the internal capture-cell handle.
/// `NativeState` is deliberately **not** here:
/// it is an opaque handle to storage a copy could not duplicate, so a type
/// holding one moves however scalar-shaped the handle is.
fn is_leaf_copyable(ty: Type) -> bool {
    matches!(
        ty,
        Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::Void
            | Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::Cell(_)
            | Type::CString
    )
}
