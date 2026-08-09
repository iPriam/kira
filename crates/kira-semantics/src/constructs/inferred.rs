//! Inferred stored-member types for construct-backed declarations.
//!
//! Construct declarations are collected before function bodies so their
//! struct ids can participate in signatures and family variants. An omitted
//! stored-member type therefore starts as `Error` and is filled once signatures
//! exist, when its initializer can be analyzed with the complete program
//! surface available.

use kira_semantics_model::{StructId, Type};
use kira_source::SourceId;
use kira_syntax_model::ast::{ConstructDecl, ConstructKind};

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Infers every unannotated stored field of a construct-backed declaration.
    ///
    /// Initializers are analyzed in declaration order in an isolated scope.
    /// Once a field is reached, its name is bound for subsequent fields, so a
    /// later initializer may use an earlier one without capturing a caller
    /// local. The HIR is used only for this type pass; construction sites
    /// reanalyze the syntax in their own per-instance field scope.
    pub(crate) fn resolve_construct_field_types(&mut self) {
        let rows: Vec<(SourceId, &ConstructDecl, StructId)> = self
            .backed_declarations()
            .into_iter()
            .filter_map(|(source, declaration)| {
                let name = self.interner.resolve(declaration.name);
                let owner = self.imports.package_of(source);
                let id = self.program.types.structs().lookup_owned(owner, name)?;
                Some((source, declaration, id))
            })
            .collect();

        for (source, declaration, id) in rows {
            let ConstructKind::Backed { params, .. } = &declaration.kind else {
                continue;
            };
            let Some(mut fields) = self
                .program
                .types
                .structs()
                .get(id)
                .map(|definition| definition.fields.clone())
            else {
                continue;
            };
            let field_offset = params.len();
            let mut declaration_ctx = FnCtx::new(Type::Void);
            declaration_ctx.push_isolated_scope();

            // Construction parameters are initialized before stored members,
            // so member defaults may use them just like earlier fields.
            for (param_index, param) in params.iter().enumerate() {
                let name = self.interner.resolve(param.name).to_owned();
                let ty = fields
                    .get(param_index)
                    .map_or(Type::Error, |field| field.ty);
                declaration_ctx.declare(&name, ty, false);
            }

            for (member_index, member) in declaration.fields.iter().enumerate() {
                let field_index = field_offset + member_index;
                let Some(declared) = fields.get(field_index).map(|field| field.ty) else {
                    continue;
                };
                let member_name = self.interner.resolve(member.name).to_owned();
                if member.slot {
                    declaration_ctx.declare(&member_name, declared, false);
                    continue;
                }
                let field_ty = if let Some(syntax) = member.default {
                    let before = self.diagnostics.len();
                    let previous_source = self.source;
                    self.source = source;
                    let resolved = self.analyze_expr_expecting(
                        &mut declaration_ctx,
                        syntax,
                        member.ty.map(|_| declared),
                    );
                    self.source = previous_source;
                    let actual = self.program.expr(resolved).type_of();
                    if member.ty.is_none() {
                        if actual == Type::Error {
                            if self.diagnostics.len() == before {
                                self.source = source;
                                self.emit(
                                    self.tree.expr(syntax).span(),
                                    "KSEM261",
                                    format!(
                                        "the type of stored construct member `{member_name}` cannot be inferred"
                                    ),
                                );
                                self.source = previous_source;
                            }
                        } else if let Some(field) = fields.get_mut(field_index) {
                            field.ty = actual;
                        }
                    } else if actual != Type::Error && !self.admits(actual, declared) {
                        self.source = source;
                        self.emit(
                            member.name_span,
                            "KSEM262",
                            format!(
                                "stored construct member `{member_name}` expects `{}`, found `{}`",
                                self.type_name(declared),
                                self.type_name(actual)
                            ),
                        );
                        self.source = previous_source;
                    }
                    fields.get(field_index).map_or(declared, |field| field.ty)
                } else {
                    if member.ty.is_none() {
                        let previous_source = self.source;
                        self.source = source;
                        self.emit(
                            member.name_span,
                            "KSEM261",
                            format!(
                                "stored construct member `{member_name}` needs a type annotation or an initializer"
                            ),
                        );
                        self.source = previous_source;
                    }
                    declared
                };
                declaration_ctx.declare(&member_name, field_ty, false);
            }
            declaration_ctx.pop_scope();
            self.program.types.structs_mut().set_fields(id, fields);
        }
    }
}
