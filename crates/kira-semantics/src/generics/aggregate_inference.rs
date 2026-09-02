use super::*;

pub(super) struct GenericInferenceField {
    pub(super) name: String,
    pub(super) type_ref: TypeRefId,
    pub(super) substitution: Vec<Vec<(String, TypeRefId)>>,
    pub(super) has_default: bool,
}

impl<'a> Analyzer<'a> {
    /// Collects the source-level field types a generic aggregate contributes to
    /// a construction. Generic class parents are walked with the type
    /// references supplied by the child (`Parent<Value>`), so values passed to
    /// an inherited required field can infer the child's parameter too.
    pub(super) fn generic_inference_fields(
        &self,
        template: GenericAggregate<'a>,
    ) -> Vec<GenericInferenceField> {
        let mut fields = Vec::new();
        self.collect_generic_inference_fields(template, &[], &mut fields, 0);
        fields
    }

    fn collect_generic_inference_fields(
        &self,
        template: GenericAggregate<'a>,
        substitution: &[Vec<(String, TypeRefId)>],
        fields: &mut Vec<GenericInferenceField>,
        depth: u32,
    ) {
        if depth >= MAX_INSTANTIATION_DEPTH {
            return;
        }
        match template {
            GenericAggregate::Struct { decl, .. } => {
                fields.extend(decl.fields.iter().map(|field| GenericInferenceField {
                    name: self.interner.resolve(field.name).to_owned(),
                    type_ref: field.ty,
                    substitution: substitution.to_vec(),
                    has_default: field.default.is_some(),
                }));
            }
            GenericAggregate::Class { decl, .. } => {
                for parent in &decl.parents {
                    let parent_name = self.interner.resolve(parent.name);
                    let Some(parent_template) = self.generic_aggregate_named(parent_name)
                    else {
                        continue;
                    };
                    if parent.type_args.len() != parent_template.type_params().len() {
                        continue;
                    }
                    let parent_substitution: Vec<(String, TypeRefId)> = parent_template
                        .type_params()
                        .iter()
                        .zip(parent.type_args.iter().copied())
                        .map(|(parameter, argument)| {
                            (self.interner.resolve(parameter.name).to_owned(), argument)
                        })
                        .collect();
                    let mut layers = Vec::with_capacity(substitution.len() + 1);
                    layers.push(parent_substitution);
                    layers.extend(substitution.iter().cloned());
                    self.collect_generic_inference_fields(
                        parent_template,
                        &layers,
                        fields,
                        depth + 1,
                    );
                }
                let own_start = fields.len();
                fields.extend(decl.fields.iter().map(|field| GenericInferenceField {
                    name: self.interner.resolve(field.name).to_owned(),
                    type_ref: field.ty,
                    substitution: substitution.to_vec(),
                    has_default: field.default.is_some(),
                }));
                // `override let` changes whether an inherited slot is required,
                // and an override on an own field is already a declaration
                // error. Mark the unique inherited name as defaulted here so a
                // class constructor's positional slots stay aligned.
                for override_field in &decl.overrides {
                    let name = self.interner.resolve(override_field.name);
                    let matches: Vec<usize> = fields
                        .iter()
                        .enumerate()
                        .filter(|(_, field)| field.name == name)
                        .map(|(index, _)| index)
                        .collect();
                    if matches.len() == 1 && matches[0] < own_start {
                        fields[matches[0]].has_default = true;
                    }
                }
            }
        }
    }
}
