use super::*;

use kira_syntax_model::ast::FieldInit;

impl Analyzer<'_> {
    /// Type-checks a struct literal into a [`HirExpr::StructNew`] holding one
    /// initializer per declared field, in declaration order.
    ///
    /// A field the literal omits is filled from its declared default, so
    /// nothing downstream of analysis has to know that defaults exist. A field
    /// with neither an initializer nor a default is the one case that cannot be
    /// filled, and it is reported here.
    pub(crate) fn analyze_struct_literal(
        &mut self,
        ctx: &mut FnCtx,
        name: kira_core::Symbol,
        name_span: kira_source::Span,
        inits: &[FieldInit],
    ) -> HirExprId {
        // A module-qualified literal (`Support.Point { … }`) resolves exactly as
        // a qualified *type* reference does: the qualifier is checked against
        // this file's imports and then decides which package's declaration is
        // meant. An unimported root is reported there and the literal's own
        // fields are still analyzed so their mistakes surface.
        let written = self.interner.resolve(name).to_owned();
        let Some(qualified) = self.split_module_qualifier(&written, name_span) else {
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let struct_name = qualified.text.clone();
        let Some(id) = self.visible_struct_qualified(&qualified) else {
            // A function of this name is the likely mistake, so say which.
            let message = if self.lookup_function(&struct_name).is_some() {
                format!("`{struct_name}` is a function, not a struct")
            } else {
                format!("unknown struct `{struct_name}`")
            };
            self.emit(name_span, "KSEM092", message);
            for init in inits {
                self.analyze_expr(ctx, init.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.link_type_name(&struct_name, name_span);
        // A C-layout struct's members are C storage, which is what lets a
        // `String` fill a `CString` member and an array fill a `RawPtr` one.
        let is_c_layout = self.ffi_c_layout_named(&struct_name).is_some();
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map_or(0, |def| def.fields.len());

        // Analyze each written initializer against the field it names, keeping
        // source order so diagnostics read in the order they were written.
        let mut slots: Vec<Option<HirExprId>> = vec![None; field_count];
        for init in inits {
            let field_name = self.interner.resolve(init.name).to_owned();
            // The field is resolved before its value, so the field's type is
            // the value's expected type: `H { values = [] }` needs it.
            let resolved = self.resolve_field(Type::Struct(id), &field_name, init.name_span);
            if resolved.is_some() {
                self.link_field_name(&struct_name, &field_name, init.name_span);
            }
            let value = self.analyze_expr_expecting(ctx, init.value, resolved.map(|(_, ty)| ty));
            let value_ty = self.program.expr(value).type_of();
            let Some((index, field_ty)) = resolved else {
                continue;
            };
            if slots[index as usize].is_some() {
                self.emit(
                    init.name_span,
                    "KSEM093",
                    format!("field `{field_name}` is initialized twice"),
                );
                continue;
            }
            // Two coercions fill a C-layout member with C storage this side
            // writes. A `String` filling a `CString` member copies its bytes
            // out; and a POINTER member — `RawPtr`, or an `@FFI.Pointer` naming
            // what it addresses — is filled from an array of seam scalars, from
            // the struct it points at, or from an `@FFI.Array` of that struct.
            // That is what a descriptor carrying a data pointer (`sg_range`) or
            // an item list beside a count (`WGPUVertexBufferLayout.attributes`)
            // is, and both are the shape a graphics API asks for.
            // The storage outlives the call, for the reason
            // `kira_runtime_abi::c_storage` gives: the callee may hold on to it.
            let pointer_fill = (is_c_layout
                && matches!(field_ty, Type::RawPtr | Type::ForeignPtr(_)))
            .then(|| self.foreign_pointer_fill(value, field_ty, init.span))
            .flatten();
            let value = if field_ty == Type::CString && value_ty == Type::String {
                self.program
                    .exprs
                    .alloc(HirExpr::CStringNew { text: value })
            } else if let Some(elements) = pointer_fill {
                elements
            } else {
                if !self.admits(value_ty, field_ty) {
                    self.emit(
                        init.span,
                        "KSEM094",
                        format!(
                            "field `{field_name}` of `{struct_name}` expects `{}`, found `{}`",
                            self.type_name(field_ty),
                            self.type_name(value_ty)
                        ),
                    );
                }
                self.coerce_into(value, field_ty)
            };
            slots[index as usize] = Some(value);
        }

        // Fill what the literal left out. A `@FFI.Struct { layout: c }` starts
        // from a zeroed value, so an omitted field with no default takes its
        // zero rather than being reported missing — the oracle's construction
        // rule.
        let mut fields = Vec::with_capacity(field_count);
        let mut missing: Vec<String> = Vec::new();
        for index in 0..field_count as u32 {
            if let Some(value) = slots[index as usize] {
                fields.push(value);
                continue;
            }
            match self.resolve_field_default_at(ctx, id, index) {
                Some(default) => fields.push(default),
                None if is_c_layout => {
                    fields.push(self.ffi_zero_field(id, index, name_span));
                }
                None => {
                    let field_name = self
                        .program
                        .types
                        .structs()
                        .get(id)
                        .and_then(|def| def.field(index))
                        .map_or_else(String::new, |field| field.name.clone());
                    missing.push(field_name);
                    fields.push(self.program.exprs.alloc(HirExpr::Error));
                }
            }
        }
        if !missing.is_empty() {
            self.emit(
                name_span,
                "KSEM095",
                format!(
                    "`{struct_name}` is missing {}: {} (no default is declared)",
                    if missing.len() == 1 {
                        "field"
                    } else {
                        "fields"
                    },
                    missing.join(", ")
                ),
            );
        }
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }
}
