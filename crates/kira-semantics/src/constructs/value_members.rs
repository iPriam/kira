//! Reading a construct family's `@Required let` members off a family value.
//!
//! A family states a *value* obligation (`@Required let label: String`), and a
//! backed declaration discharges it however it likes: a stored field or a
//! computed member. Neither shape is the family's business, so a read through
//! the family value cannot be a plain field projection — it is a synthesized tag
//! dispatcher whose arms differ from one another, which is what separates this
//! from the method dispatch in [`super::dispatch`].
//!
//! Split out of that module on the file-size ladder; the two share the same
//! shape (reserve on first use, fill once at the end) and nothing else.

use kira_semantics_model::hir::{
    CallableSignature, Callee, FuncId, HirBinaryOp, HirExpr, HirExprId, HirFunction, HirStmt,
    HirStmtId, LocalId,
};
use kira_semantics_model::{EnumId, Type};
use kira_source::Span;

use super::ConstructVariant;
use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// The type of a family's value member — `@Required let` or typed stored
    /// member — when it has one by that name.
    pub(crate) fn construct_family_field_member(&self, id: EnumId, name: &str) -> Option<Type> {
        let family = self.construct_family_names.get(&id)?;
        let member = self
            .construct_families
            .get(family)?
            .field_members
            .get(name)?;
        Some(member.result)
    }

    /// The family's name, when `name` is one of its stored members declared
    /// with no written type — the one kind of member a family value cannot
    /// read, diagnosed as `KSEM271`.
    pub(crate) fn construct_family_untyped_member(&self, id: EnumId, name: &str) -> Option<String> {
        let family = self.construct_family_names.get(&id)?;
        let info = self.construct_families.get(family)?;
        info.stored_fields
            .iter()
            .any(|field| field.name == name && field.ty.is_none())
            .then(|| family.clone())
    }

    /// Reads a family's `@Required let` member off a family value.
    ///
    /// The read is a call to a synthesized dispatcher, exactly as a computed
    /// member's is — the difference is only what each arm does once it has the
    /// concrete value, which [`Analyzer::family_field_source`] decides.
    pub(crate) fn analyze_construct_family_field(
        &mut self,
        receiver: HirExprId,
        family_id: EnumId,
        member: &str,
        result: Type,
    ) -> HirExprId {
        let Some(dispatcher) = self.family_field_dispatcher_for(family_id, member) else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: vec![receiver],
            ty: result,
            writebacks: Vec::new(),
        })
    }

    fn family_field_dispatcher_for(&mut self, family_id: EnumId, member: &str) -> Option<FuncId> {
        let family = self.construct_family_names.get(&family_id)?.clone();
        if let Some(dispatcher) = self
            .construct_families
            .get(&family)
            .and_then(|info| info.field_members.get(member))
            .and_then(|member| member.dispatcher)
        {
            return Some(dispatcher);
        }
        let dispatcher = self.reserve_synth();
        self.construct_families
            .get_mut(&family)?
            .field_members
            .get_mut(member)?
            .dispatcher = Some(dispatcher);
        Some(dispatcher)
    }

    /// Fills every family value-member dispatcher reserved by a read site.
    pub(crate) fn build_family_field_dispatchers(&mut self) {
        let mut field_rows: Vec<_> = self
            .construct_families
            .iter()
            .flat_map(|(family, info)| {
                info.field_members.iter().filter_map(move |(name, member)| {
                    member
                        .dispatcher
                        .map(|dispatcher| (dispatcher, family.clone(), name.clone()))
                })
            })
            .collect();
        field_rows.sort_by_key(|(dispatcher, _, _)| dispatcher.0);
        field_rows.retain(|(dispatcher, _, _)| self.synth_needs_body(*dispatcher));
        for (dispatcher, family, member) in field_rows {
            let function = self.construct_field_dispatcher_body(&family, &member);
            self.fill_synth(dispatcher, function);
        }
    }

    /// Builds the tag dispatcher that reads one `@Required let` family member.
    ///
    /// Each arm projects the concrete declaration out of the family value and
    /// then does whatever *that* declaration chose: call its computed member, or
    /// read its stored field. A declaration presenting neither cannot reach
    /// here — `KSEM201` refuses it at the declaration — but if one ever did, the
    /// arm returns the result type's default rather than falling through to
    /// another variant's answer.
    fn construct_field_dispatcher_body(&mut self, family: &str, member: &str) -> HirFunction {
        let Some((enum_id, variants, result)) =
            self.construct_families.get(family).and_then(|info| {
                let field = info.field_members.get(member)?;
                Some((info.enum_id, info.variants.clone(), field.result))
            })
        else {
            return self.empty_construct_dispatcher();
        };
        let mut ctx = FnCtx::new(result);
        let receiver = ctx.declare_hidden(Type::Enum(enum_id), false);

        let mut body = Vec::with_capacity(variants.len().max(1));
        for (index, variant) in variants.iter().enumerate() {
            let arm = self.family_field_arm(receiver, enum_id, *variant, member, result);
            if index + 1 == variants.len() {
                body.extend(arm);
                break;
            }
            let value = self.program.exprs.alloc(HirExpr::Local {
                local: receiver,
                ty: Type::Enum(enum_id),
            });
            let tag = self.program.exprs.alloc(HirExpr::EnumTag { value });
            let wanted = self
                .program
                .exprs
                .alloc(HirExpr::Int(i64::from(variant.tag)));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::EqInt,
                lhs: tag,
                rhs: wanted,
                ty: Type::Bool,
            });
            body.push(self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: arm,
                else_body: Vec::new(),
            }));
        }
        if body.is_empty() {
            let value = self.default_value(result);
            body.push(
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(value) }),
            );
        }
        HirFunction {
            name: format!("some {family}.{member}$read"),
            param_count: 1,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            is_main_thread: false,
            is_async: false,
            execution: kira_semantics_model::Execution::Inherited,
            mutates_self: false,
            name_span: Span::new(0, 0),
            signature: CallableSignature::synthesized(&[], result),
        }
    }

    /// One arm of a field-member dispatcher: project the concrete value, then
    /// read the member the way that declaration provides it.
    fn family_field_arm(
        &mut self,
        receiver: LocalId,
        family: EnumId,
        variant: ConstructVariant,
        member: &str,
        result: Type,
    ) -> Vec<HirStmtId> {
        let family_value = self.program.exprs.alloc(HirExpr::Local {
            local: receiver,
            ty: Type::Enum(family),
        });
        let Some(struct_id) = variant.struct_id() else {
            let value = self.default_value(result);
            return vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(value) }),
            ];
        };
        let concrete_ty = Type::Struct(struct_id);
        let concrete = self.program.exprs.alloc(HirExpr::EnumPayload {
            value: family_value,
            ty: concrete_ty,
        });
        // A computed member wins over a stored field of the same name: it is
        // the member the declaration wrote, and the two never coexist (a
        // duplicate member is `KSEM202`).
        let owner = self.member_owner_name(concrete_ty);
        let value = match self.lookup_function(&format!("{owner}.{member}")) {
            // Typed with what the member actually presents, which a child family
            // may have made more specific than this family promised; the arm
            // carries it up to the declared result below.
            Some((target, _, presented)) => self.program.exprs.alloc(HirExpr::Call {
                callee: Callee::User(target),
                args: vec![concrete],
                ty: presented,
                writebacks: Vec::new(),
            }),
            None => match self
                .program
                .types
                .structs()
                .get(struct_id)
                .and_then(|def| def.field_index(member).map(|index| (index, def)))
                .and_then(|(index, def)| def.field(index).map(|field| (index, field.ty)))
            {
                Some((index, ty)) => self.program.exprs.alloc(HirExpr::Field {
                    base: concrete,
                    index,
                    ty,
                }),
                None => self.default_value(result),
            },
        };
        let value = self.coerce_construct_value(value, Some(result));
        let value = self.coerce_into(value, result);
        vec![
            self.program
                .stmts
                .alloc(HirStmt::Return { value: Some(value) }),
        ]
    }
}
