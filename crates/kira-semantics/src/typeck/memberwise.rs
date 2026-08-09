//! A data struct's implicit memberwise constructor: `Point(x, y)` and
//! `Point(x: .., y: ..)`.
//!
//! Split out of [`super::calls`] because it is its own construction surface,
//! distinct from the `Point { x: .., y: .. }` literal that lives there and from
//! the class, construct-backed, and `@FFI.Struct` constructors that own their
//! own paths. Like all of them it produces the same [`HirExpr::StructNew`] a
//! literal does, so nothing downstream of analysis learns the constructor was
//! written as a call.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_syntax_model::ast::CallArg;

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// The struct `name` names when it is an ordinary data struct — not a class,
    /// a construct-backed declaration, or an `@FFI.Struct`, each of which owns
    /// its own construction path and is handled before this one.
    pub(crate) fn plain_struct_named(&self, name: &str) -> Option<StructId> {
        let id = self.visible_struct(name)?;
        let specialized = self.classes.contains_key(&id)
            || self.constructs.contains_key(&id)
            || self.ffi_struct_kind(id).is_some();
        (!specialized).then_some(id)
    }

    /// Type-checks `Point(x, y)` and `Point(x: .., y: ..)`: a data struct's
    /// implicit memberwise constructor.
    ///
    /// A positional argument fills the next field in declaration order; a
    /// labeled argument fills the field of that name — so `Point(1.0, 2.0)` and
    /// `Point(x: 1.0, y: 2.0)` mean the same thing, the oracle's memberwise
    /// construction. Each field the call does not reach takes its declared
    /// default, so `Mat4()` is the all-defaulted value. A field with no argument
    /// and no default is reported missing. The result is the same
    /// [`HirExpr::StructNew`] a `Point { x: .., y: .. }` literal produces, so the
    /// IR and every backend stay unaware the constructor was written as a call.
    pub(crate) fn analyze_struct_memberwise_new(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        args: &[CallArg],
        span: kira_source::Span,
    ) -> HirExprId {
        let name = self.program.types.type_name(Type::Struct(id));
        // The name and type of every field, in declaration order — the slots a
        // memberwise call fills, whether by position or by label.
        let slots: Vec<(String, Type)> = {
            let structs = self.program.types.structs();
            structs.get(id).map_or_else(Vec::new, |def| {
                def.fields
                    .iter()
                    .map(|field| (field.name.clone(), field.ty))
                    .collect()
            })
        };
        let field_count = slots.len();

        let mut initializers: Vec<Option<HirExprId>> = vec![None; field_count];
        let mut next_positional = 0usize;
        for arg in args {
            let expected_slot = match arg.label {
                Some(label) => {
                    let label = self.interner.resolve(label).to_owned();
                    slots.iter().position(|(field, _)| *field == label)
                }
                None => {
                    let slot = next_positional;
                    next_positional += 1;
                    (slot < field_count).then_some(slot)
                }
            };
            let expected = expected_slot.map(|slot| slots[slot].1);
            let value = self.analyze_expr_expecting(ctx, arg.value, expected);
            let slot = match (arg.label, expected_slot) {
                (Some(label), None) => {
                    let label = self.interner.resolve(label).to_owned();
                    self.emit(
                        arg.label_span.unwrap_or(span),
                        "KSEM226",
                        format!("`{name}` has no field named `{label}`"),
                    );
                    continue;
                }
                (None, None) => {
                    self.emit(
                        span,
                        "KSEM223",
                        format!(
                            "`{name}` has {field_count} field(s), but more value(s) were given"
                        ),
                    );
                    continue;
                }
                (_, Some(slot)) => slot,
            };
            if initializers[slot].is_some() {
                let (field, _) = &slots[slot];
                self.emit(
                    span,
                    "KSEM227",
                    format!("field `{field}` of `{name}` is set more than once"),
                );
            }
            let (field, expected) = &slots[slot];
            let actual = self.program.expr(value).type_of();
            if !self.admits(actual, *expected) {
                self.emit(
                    span,
                    "KSEM224",
                    format!(
                        "field `{field}` of `{name}` expects `{}`, found `{}`",
                        self.type_name(*expected),
                        self.type_name(actual)
                    ),
                );
            }
            let expected = *expected;
            initializers[slot] = Some(self.coerce_into(value, expected));
        }

        let mut missing: Vec<String> = Vec::new();
        for index in 0..field_count as u32 {
            if initializers[index as usize].is_some() {
                continue;
            }
            match self.resolve_field_default_at(ctx, id, index) {
                Some(default) => initializers[index as usize] = Some(default),
                None => {
                    missing.push(slots[index as usize].0.clone());
                    initializers[index as usize] = Some(self.program.exprs.alloc(HirExpr::Error));
                }
            }
        }
        if !missing.is_empty() {
            self.emit(
                span,
                "KSEM225",
                format!(
                    "`{name}` is missing {}: {} (no argument and no default)",
                    if missing.len() == 1 {
                        "field"
                    } else {
                        "fields"
                    },
                    missing.join(", ")
                ),
            );
        }
        let fields: Vec<HirExprId> = initializers
            .into_iter()
            .map(|value| value.unwrap_or_else(|| self.program.exprs.alloc(HirExpr::Error)))
            .collect();
        self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: id,
            fields,
        })
    }
}
