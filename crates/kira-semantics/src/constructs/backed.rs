//! Defining one construct-backed declaration as a struct: its construction
//! inputs, stored members, child slots, and the family surface it inherits.
//!
//! Split from the collection pass on the file-size ladder. Collection decides
//! *which* declarations exist and runs the late resolution passes; this decides
//! what one of them *is*.

use std::collections::HashSet;

use kira_semantics_model::{FieldDef, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{ConstructDecl, ConstructField, ConstructKind, TypeRefId};

use super::{ConstructInfo, ContentSlot};
use crate::analyze::{Analyzer, FieldDefault};

impl Analyzer<'_> {
    /// Fills the already-declared struct `id` with everything the backed
    /// declaration says: its parenthesized params and `let` members as fields,
    /// its `some X` members as child slots, and the family's stored members it
    /// did not override.
    pub(super) fn define_construct(&mut self, declaration: &ConstructDecl, id: StructId) {
        let ConstructKind::Backed {
            family,
            family_span,
            params,
        } = &declaration.kind
        else {
            return;
        };
        let name = self.interner.resolve(declaration.name).to_owned();
        let written_family = self.interner.resolve(*family).to_owned();
        // The family is named as written; it is filed under its declaring
        // package, so the key is what every table below is asked with.
        let family_name = self
            .visible_family_key(&written_family)
            .unwrap_or(written_family);
        let source = self.source;

        let mut fields = Vec::new();
        let mut defaults = Vec::new();
        let mut seen = HashSet::new();
        // Method members are counted separately from fields, and by member key
        // rather than by name, because two methods sharing a name and differing
        // in what they take are two members rather than one restated.
        let mut method_keys: HashSet<String> = HashSet::new();
        for param in params {
            let field_name = self.interner.resolve(param.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, param.name_span);
            fields.push(FieldDef {
                name: field_name,
                ty: self.resolve_type_ref(param.ty),
                mutable: false,
            });
            defaults.push(
                param
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source)),
            );
        }
        let mut slots = Vec::new();
        for field in &declaration.fields {
            let field_name = self.interner.resolve(field.name).to_owned();
            self.note_duplicate_member(&mut seen, &field_name, field.name_span);
            let field_index = fields.len() as u32;
            let ty = if field.slot {
                self.resolve_slot_field(field, field_index, &field_name, &mut slots)
            } else {
                field.ty.map_or(Type::Error, |ty| self.resolve_type_ref(ty))
            };
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

        let mut computed = HashSet::new();
        let mut own_methods = HashSet::new();
        for method in &declaration.methods {
            let member = self.interner.resolve(method.function.name).to_owned();
            // `@Required` states an obligation, which only a family can do: a
            // backed declaration is where an obligation is discharged, and a
            // bodyless member there would be an implementation that does
            // nothing.
            if method.required {
                self.emit(
                    method.function.name_span,
                    "KSEM249",
                    format!(
                        "`{name}` is a declaration backed by `{family_name}`, so `{member}` \
                         implements a requirement rather than declaring one; write it as an \
                         ordinary `function` member with a body"
                    ),
                );
                continue;
            }
            // A method member is told apart by what it takes, so a declaration
            // may overload one: `scaled(by:)` and `scaled(by:plus:)` are two
            // members. Its plain name still joins `seen`, because that is what
            // discharges a family requirement and shadows a family field.
            let key = self.member_key(&member, &method.function.params);
            if !method_keys.insert(key) {
                self.emit(
                    method.function.name_span,
                    "KSEM202",
                    format!(
                        "construct member `{member}` is declared more than once with these \
                         parameters"
                    ),
                );
                continue;
            }
            seen.insert(member.clone());
            own_methods.insert(member.clone());
            if method.computed {
                computed.insert(member);
            }
        }

        // The set as of *now*: the declaration's own parameters, members, and
        // methods, before the family's stored members join the field list
        // below. That is what a family requirement is discharged by, and the
        // conformance engine reads it from here rather than from the finished
        // struct.
        let members = seen.clone();
        let family_surface = self
            .visible_family_key(&family_name)
            .and_then(|key| self.construct_families.get(&key))
            .map(|info| {
                // A uniform `extend` modifier has one shared body and is never
                // implemented per variant, so it is not part of the conformance
                // surface a backed declaration must satisfy.
                let methods = info
                    .methods
                    .iter()
                    .filter(|(_, method)| !method.uniform)
                    .map(|(name, method)| (name.clone(), method.computed))
                    .collect::<Vec<_>>();
                (info.required.clone(), methods, info.stored_fields.clone())
            });
        match family_surface {
            None => self.emit(
                *family_span,
                "KSEM200",
                format!("`{name}` is backed by unknown construct family `{family_name}`"),
            ),
            Some((_, methods, stored_fields)) => {
                for (method, is_computed) in &methods {
                    if !own_methods.contains(method) && *is_computed {
                        computed.insert(method.clone());
                    }
                }
                // Family stored members are real fields of every concrete
                // backed struct. A declaration's own field wins when it
                // overrides a family member; otherwise the family default is
                // copied into this struct's default row below.
                for family_field in stored_fields {
                    if seen.contains(&family_field.name) {
                        continue;
                    }
                    self.source = family_field.source;
                    let field_index = fields.len() as u32;
                    // A family's child slot is a slot of every declaration
                    // backed by it, so it is recorded here exactly as an own
                    // slot is: the declaration inherits the channel, not just
                    // the field.
                    let ty = match (family_field.slot, family_field.ty) {
                        (true, Some(written)) => self.record_slot(
                            written,
                            field_index,
                            &family_field.name,
                            family_field.default.is_some(),
                            &mut slots,
                        ),
                        (true, None) => {
                            self.emit(
                                *family_span,
                                "KSEM261",
                                format!(
                                    "child slot `{}` inherited from `{family_name}` must declare \
                                     its element type",
                                    family_field.name
                                ),
                            );
                            Type::Error
                        }
                        (false, written) => written
                            .map(|written| self.resolve_type_ref(written))
                            .unwrap_or(Type::Error),
                    };
                    fields.push(FieldDef {
                        name: family_field.name.clone(),
                        ty,
                        mutable: family_field.mutable,
                    });
                    defaults.push(
                        family_field
                            .default
                            .map(|syntax| FieldDefault::new(syntax, family_field.source)),
                    );
                    seen.insert(family_field.name);
                }
                self.source = source;
            }
        }
        let families = self.register_family_variant(&family_name, id);

        self.program.types.structs_mut().set_fields(id, fields);
        // The slot was reserved when the id was minted; filling it by index is
        // what keeps a function-type struct minted between the two passes from
        // shifting every construct's defaults after it.
        if let Some(slot) = self.struct_defaults.get_mut(id.index() as usize) {
            *slot = defaults;
        }
        self.constructs.insert(
            id,
            ConstructInfo {
                computed,
                family: family_name,
                members,
                own_methods,
                slots,
                families,
            },
        );
        self.refuse_deferred(declaration);
    }

    /// Records a declaration's own child slot, refusing one that wrote no
    /// element type.
    fn resolve_slot_field(
        &mut self,
        field: &ConstructField,
        field_index: u32,
        field_name: &str,
        slots: &mut Vec<ContentSlot>,
    ) -> Type {
        let Some(type_ref) = field.ty else {
            self.emit(
                field.name_span,
                "KSEM261",
                format!("child slot `{field_name}` must declare its element type"),
            );
            return Type::Error;
        };
        self.record_slot(
            type_ref,
            field_index,
            field_name,
            field.default.is_some(),
            slots,
        )
    }

    /// Records one child slot from its written element type, returning the type
    /// the slot field stores.
    fn record_slot(
        &mut self,
        type_ref: TypeRefId,
        field_index: u32,
        field_name: &str,
        has_default: bool,
        slots: &mut Vec<ContentSlot>,
    ) -> Type {
        let (element_ref, list) = match self.tree.type_ref(type_ref) {
            kira_syntax_model::ast::TypeRef::Array { element, .. } => (*element, true),
            _ => (type_ref, false),
        };
        let element_ty = self.resolve_type_ref(element_ref);
        let field_ty = if list {
            self.program.types.array_of(element_ty)
        } else {
            element_ty
        };
        slots.push(ContentSlot {
            field_index,
            name: field_name.to_owned(),
            list,
            element_ty,
            field_ty,
            has_default,
        });
        field_ty
    }

    /// Records `name` among the declaration's members, reporting it when the
    /// declaration already named it.
    fn note_duplicate_member(&mut self, seen: &mut HashSet<String>, name: &str, span: Span) {
        if !seen.insert(name.to_owned()) {
            self.emit(
                span,
                "KSEM202",
                format!("construct member `{name}` is declared more than once"),
            );
        }
    }

    /// Reports every member the parser recorded as deferred rather than
    /// dropping it silently.
    pub(super) fn refuse_deferred(&mut self, declaration: &ConstructDecl) {
        for deferred in &declaration.deferred {
            self.emit(
                deferred.span,
                "KSEM203",
                format!(
                    "{} is not executable yet in a construct; the executable slice supports \
                     `@Required let`, `@Required function`, stored and computed `let` members, \
                     `body {{ … }}`, `function`/`@Consuming function` members, and \
                     `some X` / `[some X]` child slots",
                    deferred.label
                ),
            );
        }
    }
}
