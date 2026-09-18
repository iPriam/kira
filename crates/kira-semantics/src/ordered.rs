//! Structural `Ordered` conformance: whether `<`, `<=`, `>`, `>=` may compare
//! two values of a type, and why not when they may not.
//!
//! The ordering sibling of [`crate::equatable`], and deliberately stricter.
//! Equality accepts any leaf it can answer *equal or not* for — a pointer word,
//! an opaque handle, an erased `Any` — because identity is an honest answer to
//! "are these the same". Ordering has no such fallback: there is no meaningful
//! "is this handle less than that one", and an order read off an address would
//! be non-deterministic. So `Ordered` admits only the leaves that carry a
//! *total order* of their own — a number, a boolean, a string's bytes — and the
//! aggregates built from them.
//!
//! A type earns the four orderings by its shape, with no annotation, exactly as
//! it earns `==`: a struct orders field-by-field in declaration order, an array
//! element-by-element then by length, an enum by tag then payload, each leaf
//! bottoming out at a scalar, a string, or another ordered aggregate. Reaching a
//! leaf with no order is a compile error at the comparison site, naming the
//! leaf, rather than a silent fall back to identity.
//!
//! The walk mirrors [`crate::equatable`]'s classifier so the two never disagree
//! about a type's reachable shape, and it accepts precisely the leaves both the
//! VM's `Heap::compare_values` and the native backend's ordering glue compare.

use std::collections::HashSet;

use kira_semantics_model::Type;

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// Why two values of `ty` may not be ordered, naming the leaf that refuses
    /// it, or `None` when every leaf carries a total order.
    ///
    /// The gate both the aggregate ordering path and the `<T: Ordered>` bound
    /// consult before either trusts the shape: a `None` here is the promise the
    /// backend's structural three-way walk ([`HirBinaryOp::CmpValue`]) relies
    /// on.
    ///
    /// [`HirBinaryOp::CmpValue`]: kira_semantics_model::hir::HirBinaryOp::CmpValue
    pub(crate) fn ordered_refusal(&self, ty: Type) -> Option<String> {
        self.ordered_refusal_seen(ty, &mut HashSet::new())
    }

    /// [`Analyzer::ordered_refusal`], with the shapes already being examined.
    ///
    /// A type may reach itself through an array, an enum payload, or a field, so
    /// the walk records what it is deciding: a shape cannot be the reason it is
    /// itself unorderable.
    fn ordered_refusal_seen(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<String> {
        match ty {
            // An unsigned integer is refused, and not for want of an order — it
            // is that the structural walk cannot apply the right one. The value
            // the walk compares is width-erased on every backend (the VM holds
            // one `i64`, and `CmpValue` carries no type), so it cannot tell a
            // `U64` from an `Int` to know whether to compare signed or unsigned —
            // and a bare `u64Value < other` gets that right only because the
            // *operator* it compiles to carries the signedness the walk lacks.
            // Ordering the field as a signed `Int`, or comparing it directly,
            // both keep the answer correct.
            Type::Int(_) if ty.is_unsigned_int() => Some(
                "it is an unsigned integer, and a structural order cannot recover the signedness \
                 the comparison needs — the walked value is width-erased. Order it as a signed \
                 `Int`, or compare the field directly"
                    .to_owned(),
            ),
            // Totally ordered leaves. A signed integer orders by its value, a
            // boolean by `false < true`, a string by its bytes, and a `Number`
            // by its exact decimal value — each a leaf both engines' ordering
            // walks compare the same way, signedness and all.
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::Number => None,
            // A distinct type is one scalar word, so it orders exactly as that
            // word does — the representation is never itself an aggregate.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.ordered_refusal_seen(representation, seen)
            }
            Type::Struct(id) => {
                if !seen.insert(ty) {
                    return None;
                }
                let def = self.program.types.structs().get(id)?;
                for field in &def.fields {
                    if let Some(reason) = self.ordered_member(&def.name, &field.name, field.ty, seen)
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
                    if let Some(reason) = self.ordered_member(&def.name, &variant.name, payload, seen)
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
                    .and_then(|element| self.ordered_refusal_seen(element, seen))
                    .map(|reason| format!("its elements cannot be ordered: {reason}"))
            }
            // Leaves with no total order. A pointer word, an opaque handle, and
            // bare native state name storage this side cannot rank; an `Any`
            // carries a structure but no promise its own leaves are ordered; a
            // task handle, a runtime descriptor, C storage, and `Void` have no
            // order to read. Each refuses rather than ordering by address.
            Type::RawPtr => Some(
                "it is a pointer word, whose numeric address is not a meaningful order".to_owned(),
            ),
            Type::ForeignPtr(_) => Some(
                "it is a foreign pointer, whose address is not a meaningful order".to_owned(),
            ),
            Type::Cell(_) => {
                Some("it is a cell, which has reference identity, not an order".to_owned())
            }
            Type::NativeState(_) => Some(
                "it is native state, an opaque handle this side cannot rank".to_owned(),
            ),
            Type::Any => Some(
                "it is `Any`, whose erased contents carry no promise of a total order".to_owned(),
            ),
            Type::Task(_) | Type::MainThreadTask(_) => {
                Some("it is a task handle, which is joined or detached, not ordered".to_owned())
            }
            Type::RuntimeType => Some(
                "it is a runtime type descriptor, which names an identity, not an order".to_owned(),
            ),
            Type::CString => Some("it is a C string, which has no structural order".to_owned()),
            Type::CBlock => Some("it is C storage, which this side cannot read".to_owned()),
            Type::Void | Type::Error => Some("it has no value to order".to_owned()),
        }
    }

    /// Why one member makes its owner unorderable, and which member.
    ///
    /// A struct or an enum answers from inside itself, so the *inner* leaf is
    /// what the diagnostic names — that is where the fix goes. Every other
    /// unorderable member names itself and its type.
    fn ordered_member(
        &self,
        owner: &str,
        member: &str,
        ty: Type,
        seen: &mut HashSet<Type>,
    ) -> Option<String> {
        if matches!(ty, Type::Struct(_) | Type::Enum(_) | Type::Array(_) | Type::Distinct(_)) {
            return self.ordered_refusal_seen(ty, seen);
        }
        self.ordered_refusal_seen(ty, seen).map(|reason| {
            format!(
                "`{owner}`'s member `{member}` has type `{}`, which cannot be ordered: {reason}",
                self.type_name(ty)
            )
        })
    }
}
