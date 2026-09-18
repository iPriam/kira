//! Structural `Hashable` conformance: whether `hash(v)` may fold a value into
//! one number, and why not when it may not.
//!
//! The third structural classifier beside [`crate::equatable`] and
//! [`crate::ordered`], and it answers to equality the way a hash must: two values
//! that compare equal have to hash equal, so `Hashable` admits exactly the leaves
//! whose bits *are* their value. A scalar integer, a boolean, and a string's
//! bytes qualify; a **float does not** — `0.0` and `-0.0` are equal yet differ in
//! bits, and a `NaN` equals nothing yet has many bit patterns — so a float leaf
//! would make the hash disagree with equality. That is the same refusal the
//! Foundation `@Derive(Hashable)` macro makes, kept here so the trait and the
//! macro never disagree about what a hashable shape is.
//!
//! A type earns `hash` by its shape, with no annotation, exactly as it earns `==`
//! and the orderings: a struct folds its fields in declaration order, an array
//! its length then its elements, an enum its tag then its payload, each leaf
//! bottoming out at an integer, a boolean, a string, or a decimal `Number` whose
//! own value (not its representation) is what folds. Reaching a leaf that cannot
//! hash consistently with equality is a compile error at the call, naming it.

use std::collections::HashSet;

use kira_semantics_model::Type;

use crate::analyze::Analyzer;

impl Analyzer<'_> {
    /// Why `hash(v)` may not fold a value of `ty`, naming the leaf that refuses
    /// it, or `None` when every leaf hashes consistently with equality.
    ///
    /// The gate both the `hash` builtin and the `<T: Hashable>` bound consult:
    /// a `None` here is the promise the structural fold relies on.
    pub(crate) fn hashable_refusal(&self, ty: Type) -> Option<String> {
        self.hashable_refusal_seen(ty, &mut HashSet::new())
    }

    /// [`Analyzer::hashable_refusal`], with the shapes already being examined, so
    /// a type reaching itself through a field, an element, or a payload is not
    /// the reason it is itself unhashable.
    fn hashable_refusal_seen(&self, ty: Type, seen: &mut HashSet<Type>) -> Option<String> {
        match ty {
            // A float cannot be folded: equal values may carry different bits, so
            // a hash of the bits would disagree with `==`. The one refusal that
            // is about equality rather than about the leaf having no value.
            Type::Float(_) => Some(
                "it is a float, and equal floats may carry different bits (`0.0` and `-0.0`, the \
                 many `NaN`s), so hashing the bits would disagree with `==`. Hash the parts you \
                 mean by hand"
                    .to_owned(),
            ),
            // Hashable leaves: an integer and a boolean fold by their bits, and a
            // string by its bytes — each one whose bits *are* its identity, so a
            // hash of them agrees with equality.
            Type::Int(_) | Type::Bool | Type::String => None,
            // A decimal `Number` is refused for the same reason a float is: two
            // values equal under `==` need not share a byte form to fold, so a
            // hash of the storage could disagree with equality. Hash a stable key
            // derived from it instead.
            Type::Number => Some(
                "it is a decimal `Number`, whose equal values need not share a byte form, so a \
                 fold of its storage could disagree with `==`"
                    .to_owned(),
            ),
            // A distinct type is one scalar word, so it hashes as that word —
            // unless that word is a float, which the recursion refuses.
            Type::Distinct(_) => {
                let representation = self.program.types.representation(ty);
                self.hashable_refusal_seen(representation, seen)
            }
            Type::Struct(id) => {
                if !seen.insert(ty) {
                    return None;
                }
                let def = self.program.types.structs().get(id)?;
                for field in &def.fields {
                    if let Some(reason) =
                        self.hashable_member(&def.name, &field.name, field.ty, seen)
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
                        self.hashable_member(&def.name, &variant.name, payload, seen)
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
                    .and_then(|element| self.hashable_refusal_seen(element, seen))
                    .map(|reason| format!("its elements cannot be hashed: {reason}"))
            }
            // Leaves with no value to hash consistently, mirroring the other two
            // classifiers: a pointer word and an opaque handle have no stable
            // value across runs, an `Any` carries no promise its leaves hash, and
            // a task, a descriptor, C storage, and `Void` have nothing to fold.
            Type::RawPtr => {
                Some("it is a pointer word, whose address is not a stable value to hash".to_owned())
            }
            Type::ForeignPtr(_) => {
                Some("it is a foreign pointer, whose address is not a stable value".to_owned())
            }
            Type::Cell(_) => {
                Some("it is a cell, which has reference identity, not a value to hash".to_owned())
            }
            Type::NativeState(_) => {
                Some("it is native state, an opaque handle this side cannot fold".to_owned())
            }
            Type::Any => Some(
                "it is `Any`, whose erased contents carry no promise of a hash".to_owned(),
            ),
            Type::Task(_) | Type::MainThreadTask(_) => {
                Some("it is a task handle, which is joined or detached, not hashed".to_owned())
            }
            Type::RuntimeType => {
                Some("it is a runtime type descriptor, which names an identity, not a value".to_owned())
            }
            Type::CString => Some("it is a C string, which has no structural hash".to_owned()),
            Type::CBlock => Some("it is C storage, which this side cannot read".to_owned()),
            Type::Void | Type::Error => Some("it has no value to hash".to_owned()),
        }
    }

    /// Why one member makes its owner unhashable, and which member.
    fn hashable_member(
        &self,
        owner: &str,
        member: &str,
        ty: Type,
        seen: &mut HashSet<Type>,
    ) -> Option<String> {
        if matches!(ty, Type::Struct(_) | Type::Enum(_) | Type::Array(_) | Type::Distinct(_)) {
            return self.hashable_refusal_seen(ty, seen);
        }
        self.hashable_refusal_seen(ty, seen).map(|reason| {
            format!(
                "`{owner}`'s member `{member}` has type `{}`, which cannot be hashed: {reason}",
                self.type_name(ty)
            )
        })
    }
}
