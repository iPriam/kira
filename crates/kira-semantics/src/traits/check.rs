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
use crate::traits::Contract;
use crate::traits::markers::Marker;

/// Where one conformance was recorded, and how the type came by it.
struct ConformanceSite {
    /// The conforming type.
    ty: StructId,
    /// The file the conformance is filed under, which every refusal points
    /// into.
    source: SourceId,
    /// The span a refusal points at.
    span: Span,
    /// The construct family whose claim this conformance came from, when the
    /// declaration did not write it itself.
    via_family: Option<String>,
}

/// One requirement's resolved shape, as the trait wrote it.
pub(crate) struct RequiredShape {
    /// The member's name.
    pub(crate) name: String,
    /// The written parameters, receiver excluded.
    pub(crate) params: Vec<Type>,
    /// The written result, `Void` when the declaration wrote none.
    pub(crate) result: Type,
    /// Whether the member has no body, so the type must present one itself.
    pub(crate) required: bool,
    /// Whether the receiver is written `borrow mut self`.
    ///
    /// Part of the dispatch-facing signature: a member dispatched through an
    /// existential writes back through its receiver exactly when this holds,
    /// so an implementation disagreeing here cannot be reached through one.
    pub(crate) receiver_mutates: bool,
}

impl Analyzer<'_> {
    /// Checks every conformance the program recorded against the contract it
    /// keeps.
    ///
    /// One loop over one table, whichever kind of contract each row names: a
    /// declared trait's members, or a construct family's `@Required` surface.
    pub(crate) fn check_conformances(&mut self) {
        self.check_impl_blocks_declare_only_trait_members();
        for index in 0..self.conformances.len() {
            let (contract, site) = {
                let entry = &self.conformances[index];
                (
                    entry.contract.clone(),
                    ConformanceSite {
                        ty: entry.ty,
                        source: entry.source,
                        span: entry.span,
                        via_family: entry.via_family.clone(),
                    },
                )
            };
            let ConformanceSite {
                ty, source, span, ..
            } = site;
            let trait_name = match contract {
                Contract::Trait(name) => name,
                Contract::Family(family) => {
                    self.check_family_requirements(&family, &site);
                    continue;
                }
            };
            if trait_name == crate::traits::COPYABLE {
                self.check_copyable_claim(ty, source, span);
                continue;
            }
            if let Some(marker) = Marker::from_name(&trait_name) {
                self.check_marker_claim(marker, ty, source, span);
                continue;
            }
            self.check_supertraits_are_claimed(&trait_name, ty, source, span);
            let Some(shapes) = self.required_shapes(&trait_name) else {
                continue;
            };
            for shape in shapes {
                self.check_member(&trait_name, &site, &shape);
            }
        }
    }

    /// Checks one backed declaration against its family's `@Required` surface.
    ///
    /// A family's requirement is discharged by a member of that name — a
    /// construction parameter, a stored member, or a method — or by the
    /// declaration overriding *every* family method: a family member is what
    /// consumes a requirement, so a declaration that replaced them all left
    /// nothing to consume it.
    fn check_family_requirements(&mut self, family: &str, site: &ConformanceSite) {
        let Some(info) = self.construct_families.get(family) else {
            // A declaration backed by an unknown family is reported where it
            // was defined, and there is no surface here to check against.
            return;
        };
        let required = info.required.clone();
        // A uniform `extend` modifier has one shared body and is never
        // implemented per variant, so it is not part of the surface a backed
        // declaration must satisfy.
        let methods: Vec<String> = info
            .methods
            .iter()
            .filter(|(_, method)| !method.uniform)
            .map(|(name, _)| name.clone())
            .collect();
        let Some(construct) = self.constructs.get(&site.ty) else {
            return;
        };
        let overrides_all_methods =
            !methods.is_empty() && methods.iter().all(|it| construct.own_methods.contains(it));
        if overrides_all_methods {
            return;
        }
        let missing: Vec<String> = required
            .into_iter()
            .filter(|member| !construct.members.contains(member))
            .collect();
        let name = self.program.types.type_name(Type::Struct(site.ty));
        self.source = site.source;
        for member in missing {
            self.emit(
                site.span,
                "KSEM201",
                format!(
                    "`{name}` does not provide required member `{member}` of construct family \
                     `{family}`, and does not override every family method that can consume it"
                ),
            );
        }
    }

    /// Checks that a type claiming `trait_name` also claims every trait it
    /// requires.
    ///
    /// Direct supertraits only: each of those conformances is itself checked
    /// here, so the whole closure is covered without this pass walking it.
    fn check_supertraits_are_claimed(
        &mut self,
        trait_name: &str,
        ty: StructId,
        source: SourceId,
        span: Span,
    ) {
        let required: Vec<String> = self
            .traits
            .get(trait_name)
            .map(|declared| {
                declared
                    .supertraits
                    .iter()
                    .map(|entry| entry.name.clone())
                    .collect()
            })
            .unwrap_or_default();
        let type_name = self.program.types.type_name(Type::Struct(ty));
        for super_name in required {
            // A trait the compiler *derives* is true of a shape whether or not
            // anyone wrote it down, so the obligation is discharged by the fact
            // rather than by a second spelling of it. `Drop` is not one of
            // those: it is true exactly where a body was written.
            let unmet = match crate::traits::is_derived_trait(&super_name) {
                true => self.derived_trait_unmet(&super_name, ty),
                false => (!self.conforms_to(ty, &super_name)).then(|| {
                    format!(
                        "`{type_name}` does not conform to `{super_name}`. Add it to the \
                         conformance list, or write \
                         `extend {type_name}: {super_name} {{ … }}`."
                    )
                }),
            };
            let Some(unmet) = unmet else {
                continue;
            };
            // A name the supertrait clause could not resolve is already
            // reported there; a second refusal per conforming type would bury
            // it.
            if !crate::traits::is_builtin_trait(&super_name)
                && !self.traits.contains_key(&super_name)
            {
                continue;
            }
            self.source = source;
            self.emit(
                span,
                "KSEM310",
                format!(
                    "`{type_name}` claims `{trait_name}`, which requires `{super_name}`: {unmet}"
                ),
            );
        }
    }

    /// Why `ty` does not carry the derived trait `name`, or `None` when it
    /// does.
    pub(crate) fn derived_trait_unmet(&self, name: &str, ty: StructId) -> Option<String> {
        let type_name = self.program.types.type_name(Type::Struct(ty));
        match Marker::from_name(name) {
            Some(marker) => self.marker_reason(&type_name, Type::Struct(ty), marker),
            None => self.not_copyable_reason(&type_name, Type::Struct(ty), &mut HashSet::new()),
        }
    }

    /// The resolved shape of every member of `name`, or `None` when `name` is a
    /// compiler-known trait with no written members.
    ///
    /// Resolved against the *trait's* file: the signature is what the trait
    /// wrote, so the names in it mean what they mean there.
    pub(crate) fn required_shapes(&mut self, name: &str) -> Option<Vec<RequiredShape>> {
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
                receiver_mutates: function.receiver.is_some_and(|receiver| receiver.mutable),
            })
            .collect();
        self.source = here;
        Some(shapes)
    }

    /// Checks one trait member against what the conforming type presents.
    fn check_member(&mut self, trait_name: &str, site: &ConformanceSite, shape: &RequiredShape) {
        let ConformanceSite {
            ty, source, span, ..
        } = *site;
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
            // A family that claims a trait may answer for it: a member the
            // family declares or an `extend Family` modifier provides is
            // reachable on every declaration backed by it, so the declaration
            // owes nothing further.
            if let Some(family) = &site.via_family
                && self.family_provides(family, &shape.name)
            {
                return;
            }
            let claim = match &site.via_family {
                Some(family) => format!(
                    "`{type_name}` conforms to `{trait_name}` through construct family \
                     `{family}`, but presents no `{}`",
                    shape.name
                ),
                None => format!(
                    "`{type_name}` claims `{trait_name}` but presents no `{}`",
                    shape.name
                ),
            };
            self.emit(
                span,
                "KSEM292",
                format!(
                    "{claim}: the trait requires `{}`. Write it in `{type_name}`'s body or in an \
                     `extend {type_name}: {trait_name}` block.",
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
        // The receiver's mode is part of the dispatch-facing signature: a
        // member called through the existential writes back through its
        // receiver exactly when the requirement says it may, so an
        // implementation disagreeing here would lose or invent writes on every
        // call that did not name the type.
        let implements_mutates = self.mutates_self(*matched);
        if implements_mutates != shape.receiver_mutates {
            let (written, wanted) = match implements_mutates {
                true => ("`borrow mut self`", "`borrow self`"),
                false => ("`borrow self`", "`borrow mut self`"),
            };
            self.source = declared_source;
            self.emit(
                name_span,
                "KSEM293",
                format!(
                    "`{type_name}.{}` takes {written}, but `{trait_name}` requires {wanted}; \
                     dispatch through `{trait_name}` cannot reach them both",
                    shape.name
                ),
            );
            self.source = source;
        }
    }

    /// Whether construct family `family` presents `member` on every declaration
    /// backed by it.
    fn family_provides(&self, family: &str, member: &str) -> bool {
        self.construct_families
            .get(family)
            .is_some_and(|info| info.methods.contains_key(member))
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
