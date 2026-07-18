//! The three expressions a class adds: construction, parent qualification, and
//! bare access to an inherited member.
//!
//! None of them survive analysis. A constructor call becomes the same
//! `HirExpr::StructNew` a struct literal does, and both qualified and bare
//! member access become an ordinary field read or call — so the IR and every
//! backend stay unaware that classes exist.

use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};
use crate::classes::{Member, Qualifier};

impl Analyzer<'_> {
    /// Whether `name` names a class.
    pub(crate) fn class_named(&self, name: &str) -> Option<StructId> {
        let id = self.program.types.structs().lookup(name)?;
        self.classes.contains_key(&id).then_some(id)
    }

    /// Type-checks `ClsAccount(args)`: a class's constructor.
    ///
    /// A class is built by calling it, not with a struct literal. The arguments
    /// fill the fields that declare no default, in flattened order — parents'
    /// fields first — and every field that does declare one takes it. So
    /// `ClsAccount()` is the common shape, and a class with a default-less
    /// field is the only one that takes arguments.
    pub(crate) fn analyze_class_new(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        args: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let name = self.program.types.type_name(Type::Struct(id));
        let required = self
            .classes
            .get(&id)
            .map(|info| info.required_slots.clone())
            .unwrap_or_default();
        let arity_matches = args.len() == required.len();
        if !arity_matches {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{name}` takes {} constructor argument(s), found {}",
                    required.len(),
                    args.len()
                ),
            );
        }
        let field_count = self
            .program
            .types
            .structs()
            .get(id)
            .map(|def| def.fields.len())
            .unwrap_or_default();
        // Every slot is filled: the positional arguments where they belong, and
        // each remaining slot from its declared default. Downstream sees a
        // fully initialized struct, exactly as a struct literal produces.
        let mut initializers: Vec<Option<HirExprId>> = vec![None; field_count];
        for (index, &arg) in args.iter().enumerate() {
            let Some(&slot) = required.get(index) else {
                self.analyze_expr(ctx, arg);
                continue;
            };
            let expected = self
                .program
                .types
                .structs()
                .get(id)
                .and_then(|def| def.field(slot))
                .map(|field| field.ty)
                .unwrap_or(Type::Error);
            let value = self.analyze_expr(ctx, arg);
            let actual = self.program.expr(value).type_of();
            if !actual.assignable_to(expected) {
                self.emit(
                    span,
                    "KSEM063",
                    format!(
                        "constructor argument {} of `{name}` expects `{}`, found `{}`",
                        index + 1,
                        self.type_name(expected),
                        self.type_name(actual)
                    ),
                );
            }
            initializers[slot as usize] = Some(value);
        }
        for slot in 0..field_count as u32 {
            if initializers[slot as usize].is_some() {
                continue;
            }
            // A slot left unfilled by a call that already had the wrong number
            // of arguments is that same mistake, not a second one.
            let filled = match arity_matches {
                true => self.class_field_default(ctx, id, slot, span),
                false => self.program.exprs.alloc(HirExpr::Error),
            };
            initializers[slot as usize] = Some(filled);
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

    /// Analyzes the default written for one slot, or reports its absence.
    fn class_field_default(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        slot: u32,
        span: Span,
    ) -> HirExprId {
        match self.field_default(id, slot) {
            Some(default) => self.analyze_expr(ctx, default),
            None => {
                let name = self.program.types.type_name(Type::Struct(id));
                self.emit(
                    span,
                    "KSEM081",
                    format!("field {slot} of `{name}` has no value and no default"),
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
        }
    }

    /// The parent type `base` names, when `base` is a bare type name usable as
    /// a qualifier here.
    pub(crate) fn parent_qualifier_of(&mut self, ctx: &FnCtx, base: ExprId) -> Qualifier {
        let kira_syntax_model::ast::Expr::Name { symbol, span } = *self.tree.expr(base) else {
            return Qualifier::NotAType;
        };
        let root = self.interner.resolve(symbol).to_owned();
        // A local of the same name wins: a binding the reader can see beats a
        // type they have to look up.
        if ctx.resolve(&root).is_some() {
            return Qualifier::NotAType;
        }
        if self.program.types.structs().lookup(&root).is_none() {
            return Qualifier::NotAType;
        }
        match self.resolve_parent_qualifier(ctx, &root, span) {
            Some(id) => Qualifier::Parent(id),
            None => Qualifier::Rejected,
        }
    }

    /// Resolves `Parent.member` inside a class method, where `Parent` is a type
    /// name rather than a value.
    ///
    /// This is how the language spells "super": `ClsAccount.gross()` runs the
    /// parent's *body* against `self`, which is still the derived instance — so
    /// it reads the overridden field defaults, not the parent's. Returns `None`
    /// when the receiver is not a type name at all, leaving an ordinary field
    /// read or method call to handle it.
    pub(crate) fn resolve_parent_qualifier(
        &mut self,
        ctx: &FnCtx,
        root: &str,
        root_span: Span,
    ) -> Option<StructId> {
        // A local of the same name wins, the same way it does for a module
        // qualifier: a binding the reader can see beats a type they look up.
        if ctx.resolve(root).is_some() {
            return None;
        }
        let qualifier = self.program.types.structs().lookup(root)?;
        let Some(receiver) = ctx.receiver else {
            self.emit(
                root_span,
                "KSEM069",
                format!("`{root}` can only qualify a member inside a method of a class that inherits from it"),
            );
            return None;
        };
        let inherits = receiver == qualifier
            || self
                .classes
                .get(&receiver)
                .is_some_and(|info| info.ancestors.contains(&qualifier));
        if !inherits {
            let own = self.program.types.type_name(Type::Struct(receiver));
            self.emit(
                root_span,
                "KSEM069",
                format!("`{own}` does not inherit from `{root}`"),
            );
            return None;
        }
        Some(qualifier)
    }

    /// Type-checks `Parent.method(args)` inside a class method.
    pub(crate) fn analyze_parent_call(
        &mut self,
        ctx: &mut FnCtx,
        qualifier: StructId,
        method: &str,
        args: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let Some(receiver) = ctx.receiver else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let known = self.classes.get(&receiver).is_some_and(|info| {
            info.qualified_methods
                .contains(&(qualifier, method.to_owned()))
        });
        if !known {
            let qualifier_name = self.program.types.type_name(Type::Struct(qualifier));
            self.emit(
                span,
                "KSEM069",
                format!("`{qualifier_name}` has no method `{method}` to qualify"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        // The receiver is `self`: a parent-qualified call is still a call on
        // this instance, which is what makes it read the overridden defaults.
        let Some(local) = ctx.resolve("self") else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let self_hir = self.program.exprs.alloc(HirExpr::Local {
            local,
            ty: Type::Struct(receiver),
        });
        let target = self.parent_call_name(receiver, qualifier, method);
        self.analyze_user_call_from_syntax(ctx, &target, &[self_hir], args, span)
    }

    /// The registered name of `qualifier`'s copy of `method` for `receiver`.
    fn parent_call_name(&self, receiver: StructId, qualifier: StructId, method: &str) -> String {
        let receiver_name = self.program.types.type_name(Type::Struct(receiver));
        if self.is_most_derived(receiver, qualifier, method) {
            return format!("{receiver_name}.{method}");
        }
        let qualifier_name = self.program.types.type_name(Type::Struct(qualifier));
        format!("{receiver_name}.{qualifier_name}${method}")
    }

    /// Type-checks `Parent.field` inside a class method.
    pub(crate) fn analyze_parent_field(
        &mut self,
        ctx: &mut FnCtx,
        qualifier: StructId,
        field: &str,
        span: Span,
    ) -> HirExprId {
        let Some(receiver) = ctx.receiver else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let slot = self
            .classes
            .get(&receiver)
            .and_then(|info| info.qualified_fields.get(&(qualifier, field.to_owned())))
            .copied();
        let Some(slot) = slot else {
            let qualifier_name = self.program.types.type_name(Type::Struct(qualifier));
            self.emit(
                span,
                "KSEM069",
                format!("`{qualifier_name}` has no field `{field}` to qualify"),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let Some(local) = ctx.resolve("self") else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let ty = self
            .program
            .types
            .structs()
            .get(receiver)
            .and_then(|def| def.field(slot))
            .map(|def| def.ty)
            .unwrap_or(Type::Error);
        let base = self.program.exprs.alloc(HirExpr::Local {
            local,
            ty: Type::Struct(receiver),
        });
        self.program.exprs.alloc(HirExpr::Field {
            base,
            index: slot,
            ty,
        })
    }

    /// Reports a bare member name that several parents declare.
    ///
    /// Returns `true` when it reported, so the caller stops rather than adding
    /// a second, vaguer diagnostic about the same name.
    pub(crate) fn report_ambiguous_member(
        &mut self,
        owner: StructId,
        name: &str,
        span: Span,
        method: bool,
    ) -> bool {
        let owners = match self.classes.get(&owner) {
            Some(info) if method => match info.bare_methods.get(name) {
                Some(Member::Ambiguous(owners)) => owners.clone(),
                _ => return false,
            },
            Some(info) => match info.bare_fields.get(name) {
                Some(Member::Ambiguous(owners)) => owners.clone(),
                _ => return false,
            },
            None => return false,
        };
        let listed = owners
            .iter()
            .map(|owner| self.program.types.type_name(Type::Struct(*owner)))
            .collect::<Vec<_>>()
            .join("` and `");
        let kind = if method { "method" } else { "field" };
        let code = if method { "KSEM067" } else { "KSEM068" };
        self.emit(
            span,
            code,
            format!(
                "`{name}` is inherited from `{listed}`; qualify it to say which {kind} is meant"
            ),
        );
        true
    }
}

impl Analyzer<'_> {
    /// Resolves a bare call inside a method to one of the receiver's methods.
    ///
    /// `ping()` inside a class body means `self.ping()`, the same way bare
    /// `value` means `self.value`. A free function of the same name wins, so
    /// this only fires where nothing else would resolve — which is what keeps
    /// adding a method from capturing an existing call.
    pub(crate) fn implicit_method_call(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let owner = ctx.receiver?;
        if self.lookup_function(name).is_some() {
            return None;
        }
        // Ambiguity is a resolution *outcome*, not a miss: two parents defining
        // `ping` leaves the bare call unresolvable, and saying which parents is
        // the whole of the help.
        if self.report_ambiguous_member(owner, name, span, true) {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let qualified = format!(
            "{}.{name}",
            self.program.types.type_name(Type::Struct(owner))
        );
        self.lookup_function(&qualified)?;
        let local = ctx.resolve("self")?;
        let receiver = self.program.exprs.alloc(HirExpr::Local {
            local,
            ty: Type::Struct(owner),
        });
        Some(self.analyze_user_call_from_syntax(ctx, &qualified, &[receiver], args, span))
    }
}
