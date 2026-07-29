//! Building a class's flattened field list.
//!
//! A class's storage is its parents' slots in order, then its own — one flat
//! [`FieldDef`] list, because nothing below semantics knows classes exist. Four
//! things happen on the way there and they are all about *slots*: inheriting
//! them, rebinding a default with `override let`, qualifying two parents'
//! same-named slots apart, and recording how a bare name resolves once they are
//! numbered.
//!
//! Split out of [`super`] on the file-size ladder. The seam is the one already
//! in the design: [`super`] decides which classes exist and in what order, and
//! resolves their methods; this decides what one class *stores*.

use std::collections::HashMap;

use kira_semantics_model::{StructId, Type};
use kira_syntax_model::ast::{ClassDecl, OverrideFieldDecl};

use super::{ClassInfo, FlatField, Member};
use crate::analyze::{Analyzer, FieldDefault};
use crate::types::{AggregateKind, NameContext};

impl Analyzer<'_> {
    /// Builds the flattened field list: every parent's fields, in order, then
    /// this class's own, with `override let` rewriting an inherited default.
    pub(super) fn flatten_fields(
        &mut self,
        declaration: &ClassDecl,
        name: &str,
        parents: &[StructId],
    ) -> Vec<FlatField> {
        let mut flat: Vec<FlatField> = Vec::new();
        for parent in parents {
            self.inherit_fields(*parent, &mut flat);
        }
        for field in &declaration.fields {
            let plain = self.interner.resolve(field.name).to_owned();
            if flat.iter().any(|existing| existing.plain == plain) {
                // Redeclaring an inherited name without `override` would give
                // the class two slots spelled the same.
                self.emit(
                    field.name_span,
                    "KSEM074",
                    format!(
                        "`{plain}` is already inherited; write `override let {plain} = ...` to \
                         give it a new default"
                    ),
                );
                continue;
            }
            let context = NameContext::Field {
                owner_kind: AggregateKind::Class,
                owner: name.to_owned(),
            };
            let ty = self.resolve_type_in(field.ty, &context);
            flat.push(FlatField {
                owner: None,
                plain: plain.clone(),
                storage: plain,
                ty,
                mutable: field.mutable,
                default: field
                    .default
                    .map(|syntax| FieldDefault::new(syntax, self.source)),
            });
        }
        self.apply_overrides(declaration, &mut flat);
        self.qualify_collisions(&mut flat);
        flat
    }

    /// Copies one parent's fields into the flattened list, skipping a slot the
    /// list already carries — a diamond inherits one copy of the shared base,
    /// not two.
    fn inherit_fields(&mut self, parent: StructId, flat: &mut Vec<FlatField>) {
        let Some(def) = self.program.types.structs().get(parent) else {
            return;
        };
        let inherited: Vec<(String, Type, bool)> = def
            .fields
            .iter()
            .map(|field| (field.name.clone(), field.ty, field.mutable))
            .collect();
        // A parent that is itself a class already recorded where each slot came
        // from, including any qualification it applied; a struct parent owns
        // every slot under its written name.
        let origins: Vec<(StructId, String)> = match self.classes.get(&parent) {
            Some(info) => info.slot_origin.clone(),
            None => inherited
                .iter()
                .map(|(name, _, _)| (parent, name.clone()))
                .collect(),
        };
        for (slot, (storage, ty, mutable)) in inherited.into_iter().enumerate() {
            let (owner, plain) = match origins.get(slot) {
                Some(origin) => origin.clone(),
                None => (parent, storage.clone()),
            };
            if flat
                .iter()
                .any(|existing| existing.owner == Some(owner) && existing.plain == plain)
            {
                continue;
            }
            let default = self.field_default(parent, slot as u32);
            flat.push(FlatField {
                owner: Some(owner),
                plain,
                storage,
                ty,
                mutable,
                default,
            });
        }
    }

    /// Checks a restated `override let name: T = …` type against the slot it
    /// rebinds.
    ///
    /// Restating the inherited type is legal and inert — the slot keeps the type
    /// its declaration gave it either way. A restatement that *disagrees* is
    /// refused rather than silently ignored: an override is chosen by name, so a
    /// wrong type there means the author believed they were rebinding a
    /// different field, and honouring the inherited type would quietly do
    /// something else.
    fn check_override_type(&mut self, entry: &OverrideFieldDecl, name: &str, inherited: Type) {
        let Some(written) = entry.ty else {
            return;
        };
        let context = NameContext::Field {
            owner_kind: AggregateKind::Class,
            owner: name.to_owned(),
        };
        let ty = self.resolve_type_in(written, &context);
        // A type that failed to resolve already reported; saying it disagrees
        // with the inherited one on top of that would be noise.
        if ty == inherited || ty == Type::Error || inherited == Type::Error {
            return;
        }
        let (written_name, inherited_name) = (self.type_name(ty), self.type_name(inherited));
        self.emit(
            self.tree.type_ref(written).span(),
            "KSEM059",
            format!(
                "this `override` restates `{name}` as `{written_name}`, but the inherited field \
                 is `{inherited_name}`; an override keeps the type it rebinds"
            ),
        );
    }

    /// Applies `override let name = value`, replacing the inherited slot's
    /// default.
    fn apply_overrides(&mut self, declaration: &ClassDecl, flat: &mut [FlatField]) {
        for entry in &declaration.overrides {
            let name = self.interner.resolve(entry.name).to_owned();
            let matches: Vec<usize> = flat
                .iter()
                .enumerate()
                .filter(|(_, field)| field.plain == name)
                .map(|(index, _)| index)
                .collect();
            match matches.as_slice() {
                [] => self.emit(
                    entry.name_span,
                    "KSEM072",
                    format!("`{name}` overrides no inherited field"),
                ),
                [only] => {
                    let only = *only;
                    self.check_override_type(entry, &name, flat[only].ty);
                    flat[only].default = Some(FieldDefault::new(entry.default, self.source));
                }
                _ => self.emit(
                    entry.name_span,
                    "KSEM068",
                    format!(
                        "`{name}` is inherited from several parents, so this override is ambiguous"
                    ),
                ),
            }
        }
    }

    /// Stores every colliding name qualified (`ClsAlpha.v`), so two parents'
    /// same-named fields stay two distinct slots.
    ///
    /// `.` cannot appear in an identifier, so a qualified storage name can
    /// never collide with one a user wrote.
    fn qualify_collisions(&mut self, flat: &mut [FlatField]) {
        let mut counts: HashMap<String, usize> = HashMap::new();
        for field in flat.iter() {
            *counts.entry(field.plain.clone()).or_default() += 1;
        }
        for field in flat.iter_mut() {
            if counts.get(&field.plain).copied().unwrap_or(0) <= 1 {
                continue;
            }
            let Some(owner) = field.owner else {
                continue;
            };
            let owner = self.program.types.type_name(Type::Struct(owner));
            field.storage = format!("{owner}.{}", field.plain);
        }
    }

    /// Records how each bare field name resolves, now that slots are numbered.
    pub(super) fn record_bare_fields(&mut self, info: &mut ClassInfo) {
        let mut by_name: HashMap<&str, Vec<(StructId, u32)>> = HashMap::new();
        for (slot, (owner, plain)) in info.slot_origin.iter().enumerate() {
            by_name
                .entry(plain.as_str())
                .or_default()
                .push((*owner, slot as u32));
        }
        let resolved: Vec<(String, Member<u32>)> = by_name
            .into_iter()
            .map(|(name, slots)| {
                let member = match slots.as_slice() {
                    [(_, slot)] => Member::One(*slot),
                    several => Member::Ambiguous(several.iter().map(|(owner, _)| *owner).collect()),
                };
                (name.to_owned(), member)
            })
            .collect();
        info.bare_fields = resolved.into_iter().collect();
    }
}
