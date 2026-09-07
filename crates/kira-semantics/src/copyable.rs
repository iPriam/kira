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

/// Why a type is not copyable.
///
/// Two reasons, and they are different mistakes: a member owning storage is
/// about what the value *holds*, and a user `Drop` is about what releasing it
/// *runs*. Each names the type it was found on, so a reason reached through a
/// field is reported at the type that owns it rather than at the field that
/// merely holds it.
enum NotCopyable {
    /// A member owning storage a copy would have to clone.
    Member {
        /// The type the member belongs to.
        owner: String,
        /// The field or variant that made the type move.
        member: String,
        /// That member's type, as a user sees it.
        ty: String,
    },
    /// A type running a user `Drop` body, which a copy would run twice.
    UserDrop {
        /// The type claiming `Drop`.
        owner: String,
    },
}

impl NotCopyable {
    /// The type this reason was found on.
    fn owner(&self) -> &str {
        match self {
            NotCopyable::Member { owner, .. } | NotCopyable::UserDrop { owner } => owner,
        }
    }
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
            if let Some(reason) = self.not_copyable_reason(&name, ty, &mut seen) {
                self.emit(
                    span,
                    "KIR005",
                    format!(
                        "`{name}` derives `Copy`, but {reason}, so `{name}` moves rather than \
                         copies. Remove the derive and let it move, borrow it, or give it an \
                         explicit duplication."
                    ),
                );
            }
        }
    }

    /// Why `ty` is not copyable, phrased as a clause naming the member that
    /// owns storage, or `None` when it is copyable.
    ///
    /// `claimed` is the type the assertion was written on: an offending member
    /// of that type itself reads as "its member", and one reached through
    /// another type names the type it belongs to, which is where the fix goes.
    ///
    /// Shared by the two spellings of the same assertion — `@Derive(Copy)` and
    /// the `Copyable` trait — so both name the offending member identically and
    /// neither can drift from the other.
    pub(crate) fn not_copyable_reason(
        &self,
        claimed: &str,
        ty: Type,
        seen: &mut HashSet<Type>,
    ) -> Option<String> {
        let reason = self.not_copyable(ty, seen)?;
        let here = reason.owner() == claimed;
        Some(match &reason {
            NotCopyable::Member { owner, member, ty } => {
                let where_it_is = match here {
                    true => format!("its member `{member}`"),
                    false => format!("`{owner}`'s member `{member}`"),
                };
                format!(
                    "{where_it_is} has type `{ty}`, which is not copyable — it owns storage a \
                     copy would have to clone"
                )
            }
            NotCopyable::UserDrop { owner } => match here {
                true => "it runs a user `Drop` body, which a copy would run a second time \
                         for storage that only goes away once"
                    .to_owned(),
                false => format!(
                    "`{owner}` runs a user `Drop` body, which a copy would run a second time \
                     for storage that only goes away once"
                ),
            },
        })
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
                    // The row was declared under this file's package; the
                    // owner-scoped lookup is what tells same-named enums in
                    // different packages apart.
                    let Some(id) = self
                        .program
                        .types
                        .enums()
                        .lookup_owned(self.imports.package_of(source), &name)
                    else {
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
        // Asked of the conformance rather than of the recorded body, because a
        // `Drop` body is a method and has no id until signatures exist — later
        // than the copy question is first asked.
        if self.conforms_to(Type::Struct(id), crate::traits::DROP) {
            return Some(NotCopyable::UserDrop { owner });
        }
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
        Some(NotCopyable::Member {
            owner: owner.to_owned(),
            member: member.to_owned(),
            ty: self.type_name(ty),
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
            // A distinct type is one scalar word — its representation can be
            // nothing else — so a struct holding one is copyable exactly as a
            // struct holding the scalar is.
            | Type::Distinct(_)
            | Type::CString
    )
}

impl Analyzer<'_> {
    /// Why a value of `ty` may not be written `copy`, or `None` when it is
    /// Copyable.
    ///
    /// `copy` needs proof: a scalar, a string, an array of copyable elements,
    /// and a struct or enum whose members are copyable and that runs no user
    /// `Drop` body may be copied. Native state, a cell, a task handle, an
    /// erased `Any` (whose payload nothing here can prove copyable), and a
    /// closure whose captures are not copyable may not — each owns something
    /// a second holder would release twice or write through unseen.
    pub(crate) fn copy_refusal(&self, ty: Type) -> Option<String> {
        self.copy_refusal_seen(ty, &mut HashSet::new())
    }

    /// [`Analyzer::copy_refusal`], with the shapes already being examined.
    ///
    /// A struct may hold itself through an array or an enum, so the walk
    /// records what it is deciding: a shape cannot be the reason it is itself
    /// uncopyable.
    ///
    /// This is a different question from the one `@Derive(Copy)` asks, and
    /// deliberately a weaker one. The derive asserts that a type copies
    /// *implicitly*, so a member that owns storage disqualifies it: gaining a
    /// `String` field would otherwise silently turn free copies into clones.
    /// `copy value` is the explicit spelling of that clone, so it asks only
    /// whether duplication is defined at all — which it is for every value the
    /// language already deep-copies on bind, and is not for a value with one
    /// owner (native state, a cell, a task handle, C storage), a value whose
    /// payload cannot be proven (`Any`), or a value whose `Drop` body would run
    /// a second time.
    fn copy_refusal_seen(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<String> {
        match ty {
            Type::NativeState(_) => {
                Some("it is native state, which has exactly one owner".to_owned())
            }
            Type::Cell(_) => Some(
                "it is a cell: shared mutable storage a copy would write through unseen".to_owned(),
            ),
            Type::Task(_) | Type::MainThreadTask(_) => {
                Some("it is a task handle, which is joined or detached exactly once".to_owned())
            }
            Type::Any => Some(
                "it is an `Any`, whose payload cannot be proven copyable; downcast it with `as` \
                 first"
                    .to_owned(),
            ),
            Type::CBlock => Some("it is C storage, which has exactly one owner".to_owned()),
            Type::Array(_) => {
                if !seen.insert(ty) {
                    return None;
                }
                self.program
                    .types
                    .element_of(ty)
                    .and_then(|element| self.copy_refusal_seen(element, seen))
                    .map(|reason| format!("its elements cannot be copied: {reason}"))
            }
            Type::Struct(_) | Type::Enum(_) => self.aggregate_copy_refusal(ty, seen),
            _ => None,
        }
    }

    /// Why a struct or enum may not be written `copy`.
    ///
    /// A user `Drop` body is the aggregate's own reason; otherwise the reason
    /// belongs to the first member that has one.
    fn aggregate_copy_refusal(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<String> {
        if !seen.insert(ty) {
            return None;
        }
        let owner = self.type_name(ty);
        if self.conforms_to(ty, crate::traits::DROP) {
            return Some(format!(
                "`{owner}` runs a user `Drop` body, which a copy would run a second time for \
                 storage that only goes away once"
            ));
        }
        let members: Vec<(String, Type)> = match ty {
            Type::Struct(id) => self
                .program
                .types
                .structs()
                .get(id)?
                .fields
                .iter()
                .map(|field| (field.name.clone(), field.ty))
                .collect(),
            Type::Enum(id) => self
                .program
                .types
                .enums()
                .get(id)?
                .variants
                .iter()
                .filter_map(|variant| {
                    variant
                        .payload
                        .map(|payload| (variant.name.clone(), payload))
                })
                .collect(),
            _ => return None,
        };
        members.into_iter().find_map(|(member, member_ty)| {
            let reason = self.copy_refusal_seen(member_ty, seen)?;
            Some(format!(
                "`{owner}`'s member `{member}` cannot be copied: {reason}"
            ))
        })
    }
}
