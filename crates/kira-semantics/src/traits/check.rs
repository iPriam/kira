//! Checking that a claimed conformance is kept.
//!
//! Runs after signatures, because every question here is about *resolved*
//! shapes: whether the type presents a member of the requirement's name, and
//! whether what it presents takes and returns what the trait asked for. The
//! requirement's own types resolve in the trait's file and the implementation's
//! in its own, which is what lets a trait and a conforming type live in
//! different packages and still mean the same `Point`.

use std::collections::HashSet;

use kira_semantics_model::{StructId, Type};
use kira_source::{SourceId, Span};
use kira_syntax_model::ast::{Function, Item};

use crate::analyze::Analyzer;

/// One requirement's resolved shape, as the trait wrote it.
struct RequiredShape {
    /// The member's name.
    name: String,
    /// The written parameters, receiver excluded.
    params: Vec<Type>,
    /// The written result, `Void` when the declaration wrote none.
    result: Type,
    /// Whether the member has no body, so the type must present one itself.
    required: bool,
}

impl Analyzer<'_> {
    /// Checks every declared conformance against the trait it claims.
    pub(crate) fn check_trait_conformance(&mut self) {
        self.check_impl_blocks_declare_only_trait_members();
        for index in 0..self.conformances.len() {
            let (trait_name, ty, source, span) = {
                let entry = &self.conformances[index];
                (entry.trait_name.clone(), entry.ty, entry.source, entry.span)
            };
            if trait_name == crate::traits::COPYABLE {
                self.check_copyable_claim(ty, source, span);
                continue;
            }
            let Some(shapes) = self.required_shapes(&trait_name) else {
                continue;
            };
            for shape in shapes {
                self.check_member(&trait_name, ty, source, span, &shape);
            }
        }
    }

    /// The resolved shape of every member of `name`, or `None` when `name` is a
    /// compiler-known trait with no written members.
    ///
    /// Resolved against the *trait's* file: the signature is what the trait
    /// wrote, so the names in it mean what they mean there.
    fn required_shapes(&mut self, name: &str) -> Option<Vec<RequiredShape>> {
        let (source, members) = {
            let declared = self.traits.get(name)?;
            let members: Vec<(String, bool, &Function)> = declared
                .members
                .iter()
                .map(|member| (member.name.clone(), member.required, member.function))
                .collect();
            (declared.source, members)
        };
        let here = self.source;
        self.source = source;
        let shapes = members
            .into_iter()
            .map(|(name, required, function)| RequiredShape {
                name,
                params: function
                    .params
                    .iter()
                    .map(|param| self.resolve_type_ref(param.ty))
                    .collect(),
                result: function
                    .return_type
                    .map_or(Type::Void, |written| self.resolve_type_ref(written)),
                required,
            })
            .collect();
        self.source = here;
        Some(shapes)
    }

    /// Checks one trait member against what the conforming type presents.
    fn check_member(
        &mut self,
        trait_name: &str,
        ty: StructId,
        source: SourceId,
        span: Span,
        shape: &RequiredShape,
    ) {
        let type_name = self.program.types.type_name(Type::Struct(ty));
        let qualified = format!("{type_name}.{}", shape.name);
        let candidates: Vec<_> = self
            .sig_index
            .get(&qualified)
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .collect();
        self.source = source;
        if candidates.is_empty() {
            // A default the type did not write is registered as one of its own
            // callables, so a member that is absent here and not required lost
            // its registration to a mistake already reported.
            if !shape.required {
                return;
            }
            self.emit(
                span,
                "KSEM292",
                format!(
                    "`{type_name}` claims `{trait_name}` but presents no `{}`: the trait \
                     requires `{}`. Write it in `{type_name}`'s body or in an \
                     `extend {type_name}: {trait_name}` block.",
                    shape.name,
                    self.requirement_spelling(shape)
                ),
            );
            return;
        }
        let matched = candidates.iter().find(|id| {
            let sig = &self.sigs[id.0 as usize];
            sig.params.len() == shape.params.len() + 1 && sig.params[1..] == shape.params[..]
        });
        let Some(matched) = matched else {
            let first = candidates[0];
            let (name_span, declared_source) = {
                let sig = &self.sigs[first.0 as usize];
                (sig.name_span, sig.source)
            };
            self.source = declared_source;
            self.emit(
                name_span,
                "KSEM293",
                format!(
                    "`{type_name}.{}` does not match what `{trait_name}` requires: the trait \
                     declares `{}`",
                    shape.name,
                    self.requirement_spelling(shape)
                ),
            );
            self.source = source;
            return;
        };
        let (result, name_span, declared_source) = {
            let sig = &self.sigs[matched.0 as usize];
            (sig.return_type, sig.name_span, sig.source)
        };
        if result != shape.result && result != Type::Error && shape.result != Type::Error {
            let written = self.type_name(result);
            let wanted = self.type_name(shape.result);
            self.source = declared_source;
            self.emit(
                name_span,
                "KSEM293",
                format!(
                    "`{type_name}.{}` returns `{written}`, but `{trait_name}` requires \
                     `{wanted}`",
                    shape.name
                ),
            );
            self.source = source;
        }
    }

    /// The requirement as a reader would write it, for a diagnostic that has to
    /// name the shape it wanted.
    fn requirement_spelling(&self, shape: &RequiredShape) -> String {
        let params: Vec<String> = shape.params.iter().map(|ty| self.type_name(*ty)).collect();
        format!(
            "function {}({}) -> {}",
            shape.name,
            params.join(", "),
            self.type_name(shape.result)
        )
    }

    /// Checks a `Copyable` claim against the type's own members.
    ///
    /// The same question `@Derive(Copy)` asks, asked by the trait spelling: a
    /// type copies when every member it reaches does, and the refusal names the
    /// member that owns storage a copy would have to clone.
    fn check_copyable_claim(&mut self, ty: StructId, source: SourceId, span: Span) {
        self.source = source;
        let name = self.program.types.type_name(Type::Struct(ty));
        let mut seen = HashSet::new();
        if let Some(reason) = self.not_copyable_reason(&name, Type::Struct(ty), &mut seen) {
            self.emit(
                span,
                "KSEM297",
                format!(
                    "`{name}` claims `Copyable`, but {reason}, so `{name}` moves rather than \
                     copies. Drop the claim and let it move, borrow it, or give it an explicit \
                     duplication."
                ),
            );
        }
    }

    /// Refuses a member an impl block declares that its trait never did.
    ///
    /// An `extend T: Trait` block *is* the trait's members for `T`. A method
    /// the trait never declared would be an ordinary method smuggled in through
    /// a conformance, reachable from nowhere the trait describes.
    fn check_impl_blocks_declare_only_trait_members(&mut self) {
        let tree = self.tree;
        for (source, item) in tree.items_with_source() {
            let Item::Extend(declaration) = item else {
                continue;
            };
            let Some(claimed) = declaration.conforms else {
                continue;
            };
            self.source = source;
            let trait_name = self.interner.resolve(claimed.name).to_owned();
            // A compiler-known trait declares its members here rather than in
            // source, and the rule is the same one: a block carries the trait's
            // members and nothing else.
            let members: HashSet<String> = match trait_name.as_str() {
                crate::traits::DROP => HashSet::from([crate::traits::drop::DROP_MEMBER.to_owned()]),
                crate::traits::COPYABLE => HashSet::new(),
                _ => {
                    let Some(declared) = self.traits.get(&trait_name) else {
                        continue;
                    };
                    declared
                        .members
                        .iter()
                        .map(|member| member.name.clone())
                        .collect()
                }
            };
            for method in &declaration.methods {
                let name = self.interner.resolve(method.name).to_owned();
                if !members.contains(&name) {
                    self.emit(
                        method.name_span,
                        "KSEM294",
                        format!(
                            "`{trait_name}` declares no member `{name}`, so this block cannot \
                             implement one: an impl block carries the trait's members and \
                             nothing else"
                        ),
                    );
                }
            }
        }
    }
}
