//! Structural `Equatable` conformance: whether `==` may compare two values of a
//! type, and why not when it may not.
//!
//! Kira commits to structural equality for every type whose leaves are
//! themselves comparable — a struct compares field-by-field, an array by length
//! then element-by-element, an enum by tag then payload, each leaf bottoming out
//! at a scalar, a string, a pointer word, or another conforming aggregate. That
//! is auto-conformance: a type earns `==` by its shape, with no annotation, and
//! loses it only by holding something that cannot be compared.
//!
//! A leaf that owns a meaning `==` cannot honour — a decimal `Number` (whose
//! equality is its own operation), a task handle, C storage, an unresolved
//! runtime descriptor inside a value — makes the whole type non-comparable.
//! Reaching one is a compile error at the comparison site, naming the leaf,
//! rather than a silent fall back to identity: identity equality for a value
//! that looks structural is the mistake this refusal exists to prevent.
//!
//! The walk mirrors [`crate::copyable`]'s structural classifier exactly, so the
//! two never disagree about what a type's reachable shape is, and it accepts
//! precisely the leaves both the VM's `Heap::values_equal` and the native
//! backend's `equal_at_walk` compare — a set neither engine can drift from
//! without this refusing first.

use std::collections::HashSet;

use kira_semantics_model::Type;
use kira_semantics_model::hir::HirBinaryOp;
use kira_syntax_model::ast::BinaryOp;

use crate::analyze::Analyzer;
use crate::operators::resolve_binary;

impl Analyzer<'_> {
    /// The HIR operator that compares two values of `ty` with `==`, or `None`
    /// when the type has no equality.
    ///
    /// A scalar, a string, a pointer word, an `Any`, or a runtime descriptor
    /// gets its own direct instruction from [`resolve_binary`]; every other
    /// conforming aggregate gets [`HirBinaryOp::EqValue`], the structural walk.
    /// One place both the `==` operator and the array methods that need equality
    /// (`contains`) read, so a type that can be compared one way can be compared
    /// every way.
    pub(crate) fn equality_op(&self, ty: Type) -> Option<HirBinaryOp> {
        if let Some((op, _)) = resolve_binary(BinaryOp::Eq, ty, ty) {
            return Some(op);
        }
        (matches!(
            ty,
            Type::Struct(_) | Type::Array(_) | Type::Enum(_) | Type::Distinct(_)
        ) && self.is_equatable(ty))
        .then_some(HirBinaryOp::EqValue)
    }

    /// Whether two values of `ty` may be compared with `==` / `!=`.
    ///
    /// The gate the aggregate equality path consults before it emits an
    /// [`EqValue`](kira_semantics_model::hir::HirBinaryOp::EqValue): a `true`
    /// here is the promise the backend's structural walk relies on.
    pub(crate) fn is_equatable(&self, ty: Type) -> bool {
        self.equatable_refusal(ty).is_none()
    }

    /// Why two values of `ty` may not be compared, naming the leaf that refuses
    /// it, or `None` when every leaf is comparable.
    pub(crate) fn equatable_refusal(&self, ty: Type) -> Option<String> {
        self.equatable_refusal_seen(ty, &mut HashSet::new())
    }

    /// [`Analyzer::equatable_refusal`], with the shapes already being examined.
    ///
    /// A type may reach itself through an array, an enum payload, or a field, so
    /// the walk records what it is deciding: a shape cannot be the reason it is
    /// itself non-comparable.
    fn equatable_refusal_seen(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<String> {
        match ty {
            // Comparable leaves. Scalars and pointer words compare bit-for-bit;
            // a string by its bytes; a cell, native state, and a foreign pointer
            // by identity — the one honest answer for storage this side cannot
            // read into — and an `Any` by the structure its box already carries.
            // Each is a leaf both engines' equality walks already handle.
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            // A `Number` compares by its exact decimal value, the same equality
            // `Number == Number` uses at the top level, so it is comparable as a
            // leaf too.
            | Type::Number
            | Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::NativeState(_)
            | Type::Cell(_)
            | Type::Any => None,
            // A distinct type is one scalar word, so it compares exactly as that
            // word does — the representation is never itself an aggregate.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.equatable_refusal_seen(representation, seen)
            }
            Type::Struct(id) => {
                if !seen.insert(ty) {
                    return None;
                }
                let def = self.program.types.structs().get(id)?;
                for field in &def.fields {
                    if let Some(reason) = self.equatable_member(&def.name, &field.name, field.ty, seen)
                    {
                        return Some(reason);
                    }
                }
                None
            }
            Type::Enum(id) => {
                if !seen.insert(ty) {
                    return None;
                }
                let def = self.program.types.enums().get(id)?;
                for variant in &def.variants {
                    let Some(payload) = variant.payload else {
                        continue;
                    };
                    if let Some(reason) =
                        self.equatable_member(&def.name, &variant.name, payload, seen)
                    {
                        return Some(reason);
                    }
                }
                None
            }
            Type::Array(_) => {
                if !seen.insert(ty) {
                    return None;
                }
                self.program
                    .types
                    .element_of(ty)
                    .and_then(|element| self.equatable_refusal_seen(element, seen))
                    .map(|reason| format!("its elements cannot be compared: {reason}"))
            }
            // Non-comparable leaves — each owns a meaning `==` cannot honour.
            Type::Task(_) | Type::MainThreadTask(_) => {
                Some("it is a task handle, which is joined or detached, not compared".to_owned())
            }
            Type::RuntimeType => Some(
                "it is a runtime type descriptor; compare two of them directly rather than \
                 through the value that holds one"
                    .to_owned(),
            ),
            Type::CString => Some("it is a C string, which has no structural equality".to_owned()),
            Type::CBlock => Some("it is C storage, which this side cannot read".to_owned()),
            Type::Void | Type::Error => Some("it has no value to compare".to_owned()),
        }
    }

    /// Why one member makes its owner non-comparable, and which member.
    ///
    /// A struct or an enum answers from inside itself, so the *inner* leaf is
    /// what the diagnostic names — that is where the fix goes. Every other
    /// non-comparable member names itself and its type.
    fn equatable_member(
        &self,
        owner: &str,
        member: &str,
        ty: Type,
        seen: &mut HashSet<Type>,
    ) -> Option<String> {
        if matches!(ty, Type::Struct(_) | Type::Enum(_) | Type::Array(_) | Type::Distinct(_)) {
            return self.equatable_refusal_seen(ty, seen);
        }
        self.equatable_refusal_seen(ty, seen).map(|reason| {
            format!(
                "`{owner}`'s member `{member}` has type `{}`, which cannot be compared: {reason}",
                self.type_name(ty)
            )
        })
    }
}
