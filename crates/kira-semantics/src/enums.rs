//! Enum declaration and use: the enum table, leading-dot construction, and the
//! tag-comparison desugar for `==`/`!=`.
//!
//! # Why equality is a desugar, not a new operator
//!
//! Two enum values are equal when their discriminants are — the corpus compares
//! only payload-less variants, and the reference compares tags even for
//! payload-carrying enums. So `e == .V` is lowered here to an `Int` comparison
//! of two tags: [`crate::analyze::Analyzer::enum_tag_operand`] reads one tag off
//! each side and the existing `EqInt`/`NeInt` does the rest. No backend learns
//! that enum equality exists — it *is* integer equality by the time one sees it.
//!
//! A payload-less variant literal folds to the tag constant directly, so the
//! common `c == .Red` never allocates a throwaway enum just to read its tag.

use kira_semantics_model::hir::{HirBinaryOp, HirExpr, HirExprId};
use kira_semantics_model::{EnumDef, EnumId, Type, VariantDef};
use kira_source::{SourceId, Span};
use kira_syntax_model::SyntaxTree;
use kira_syntax_model::ast::{EnumDecl, Expr, ExprId, Item};

use crate::analyze::{Analyzer, FnCtx};
use crate::types::NameContext;

impl<'a> Analyzer<'a> {
    /// First pass: declares every enum's name with no variants yet.
    ///
    /// Runs before structs, so a struct field may name an enum. The variants
    /// wait for [`Analyzer::resolve_enum_payloads`], because a payload may name
    /// a struct — `Backdrop(Glow)` — and no struct has an id yet. Resolving both
    /// halves here is what made a struct payload unresolvable at all: the two
    /// declaration kinds each need the other, so one of them has to arrive in
    /// two parts, exactly as a struct's own fields do.
    pub(crate) fn declare_enum_headers(&mut self) -> Vec<(EnumId, &'a EnumDecl, SourceId)> {
        let tree: &'a SyntaxTree = self.tree;
        let mut headers = Vec::new();
        for (source, item) in tree.items_with_source() {
            let Item::Enum(declaration) = item else {
                continue;
            };
            // The name and its diagnostics belong to the declaring file.
            self.source = source;
            // A generic declaration names no type — it is registered as a
            // template and waits for a written instantiation to mint one.
            if self.register_generic_enum(declaration, source) {
                continue;
            }
            let name = self.interner.resolve(declaration.name).to_owned();
            match self.program.types.enums_mut().declare(EnumDef {
                name: name.clone(),
                variants: Vec::new(),
            }) {
                Some(id) => {
                    // Reserved in id order now and filled by the second pass, so
                    // `enum_defaults` stays indexed by the ids the table minted.
                    self.enum_defaults.push(Vec::new());
                    headers.push((id, declaration, source));
                }
                None => self.emit(
                    declaration.name_span,
                    "KSEM006",
                    format!("enum `{name}` is already defined"),
                ),
            }
        }
        headers
    }

    /// Second pass: resolves each declared enum's variants, now that every
    /// struct, class, and construct-backed type has an id a payload can name.
    pub(crate) fn resolve_enum_payloads(&mut self, headers: &[(EnumId, &'a EnumDecl, SourceId)]) {
        for &(id, declaration, source) in headers {
            // Payload types resolve against the imports of the declaring file.
            self.source = source;
            let name = self.interner.resolve(declaration.name).to_owned();
            let (def, defaults) = self.resolve_enum_def(declaration, name);
            self.program
                .types
                .enums_mut()
                .set_variants(id, def.variants);
            if let Some(slot) = self.enum_defaults.get_mut(id.index() as usize) {
                *slot = defaults;
            }
        }
    }

    /// Resolves one enum declaration's variants, reporting duplicates and
    /// payload types the subset cannot carry.
    ///
    /// Returns the definition and its per-variant defaults, index-aligned.
    ///
    /// `name` is passed in rather than read off the declaration because a
    /// generic instantiation declares the same body under its mangled name
    /// (`Result<Int, AppError>`), with
    /// [`Analyzer::bound_type_param`](crate::analyze::Analyzer) supplying the
    /// substitution as the payload types resolve.
    pub(crate) fn resolve_enum_def(
        &mut self,
        declaration: &EnumDecl,
        name: String,
    ) -> (EnumDef, Vec<Option<ExprId>>) {
        let mut variants: Vec<VariantDef> = Vec::with_capacity(declaration.variants.len());
        let mut defaults: Vec<Option<ExprId>> = Vec::with_capacity(declaration.variants.len());
        for variant in &declaration.variants {
            let variant_name = self.interner.resolve(variant.name).to_owned();
            if variants
                .iter()
                .any(|existing| existing.name == variant_name)
            {
                self.emit(
                    variant.name_span,
                    "KSEM007",
                    format!("enum `{name}` already has a variant named `{variant_name}`"),
                );
                continue;
            }
            let payload = variant.payload.map(|type_ref| {
                let ty = self.resolve_type_in(type_ref, &NameContext::Ordinary);
                // Inside an instantiation the payload type is whatever the
                // arguments said, so the refusal belongs to the use site.
                match self.payload_blame {
                    Some((source, span)) => {
                        let written_in = self.source;
                        self.source = source;
                        let checked = self.check_payload_type(ty, span);
                        self.source = written_in;
                        checked
                    }
                    None => self.check_payload_type(ty, self.tree.type_ref(type_ref).span()),
                }
            });
            variants.push(VariantDef {
                name: variant_name,
                payload,
            });
            defaults.push(variant.default);
        }
        (EnumDef { name, variants }, defaults)
    }

    /// Restricts an enum payload to a type the runtime box can carry.
    ///
    /// The box holds one type-erased value slot. A scalar fits directly; a
    /// `String` or nested enum is an owned handle; a struct uses an erased
    /// aggregate box with compiler-generated clone/free leaves. Arrays remain
    /// refused until their element callbacks can travel with the payload.
    ///
    /// A nested enum is admitted because `Result`-shaped values are built from
    /// one: `Error` carries the failure enum, which is what
    /// `attempt`/`try`/`handle` routes on. Every layer already reclaims it
    /// recursively — the VM's `Heap::copy_value`/`free_enum`, the native box's
    /// `EnumPayloadKind::ENUM`, and the WASM lowering's handle payload — and the
    /// recursion terminates because a payload's type resolves against types that
    /// already resolve, so a cycle is unrepresentable.
    fn check_payload_type(&mut self, ty: Type, span: Span) -> Type {
        match ty {
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::Struct(_)
            | Type::Enum(_)
            | Type::Error => ty,
            _ => {
                self.emit(
                    span,
                    "KSEM118",
                    format!(
                        "an enum payload of type `{}` is not supported yet; a payload may be \
                         `Int`, `Float`, `Bool`, `String`, a struct, or another enum",
                        self.type_name(ty)
                    ),
                );
                Type::Error
            }
        }
    }

    /// The default payload initializer written for variant `tag` of `id`, if
    /// any.
    fn variant_default(&self, id: EnumId, tag: u32) -> Option<ExprId> {
        self.enum_defaults
            .get(id.index() as usize)
            .and_then(|defaults| defaults.get(tag as usize))
            .copied()
            .flatten()
    }

    /// The enum a qualified spelling (`SizeMode`, `Foundation.SizeMode`) names
    /// at `base`, when `base` is a pure name path that resolves to one.
    ///
    /// This is what makes `SizeMode.Hug` and `Foundation.SizeMode.Fill` write a
    /// variant the way `.Hug` does with an expected type: the base names the
    /// enum, and the member after it is the variant. It is deliberately quiet —
    /// `None` when the base is not a qualified enum at all — so an ordinary
    /// field read or an undefined name still reports on its own path.
    ///
    /// A local of the same name wins over the enum, mirroring every other
    /// qualifier here. A generic enum is not resolved: a qualified spelling
    /// carries no type arguments, and constructing one needs them.
    pub(crate) fn qualified_enum_at(&self, ctx: &FnCtx, base: ExprId) -> Option<EnumId> {
        let path = self.name_path_of(base)?;
        let candidate = match path.split_once('.') {
            None => {
                if ctx.resolve(&path).is_some() {
                    return None;
                }
                path
            }
            Some((root, member)) => {
                // A module-qualified enum: strip the imported root. A local
                // named like the root wins, and a further-dotted member is not
                // a single enum name.
                if ctx.resolve(root).is_some() || member.contains('.') {
                    return None;
                }
                self.module_for_root(root)?;
                member.to_owned()
            }
        };
        if self.is_generic_enum(&candidate) {
            return None;
        }
        self.program.types.enums().lookup(&candidate)
    }

    /// Reconstructs the dotted spelling of a pure name path (`A`, `A.B`), or
    /// `None` when the expression is anything else.
    fn name_path_of(&self, base: ExprId) -> Option<String> {
        match self.tree.expr(base) {
            Expr::Name { symbol, .. } => Some(self.interner.resolve(*symbol).to_owned()),
            Expr::Field { base, field, .. } => {
                let prefix = self.name_path_of(*base)?;
                Some(format!("{prefix}.{}", self.interner.resolve(*field)))
            }
            _ => None,
        }
    }

    /// Type-checks a leading-dot member (`.Red`, `.Ok(12)`) against the type
    /// expected at its position.
    ///
    /// The expected type must be an enum: that is the whole v0 meaning of a
    /// leading dot. Anything else — no expectation, or a non-enum one — is a
    /// typed refusal rather than a guess, because a leading dot against a class,
    /// a function, or a construct is surface this subset does not have.
    pub(crate) fn analyze_dot_member(
        &mut self,
        ctx: &mut FnCtx,
        name: kira_core::Symbol,
        name_span: Span,
        args: &Option<Vec<ExprId>>,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let member = self.interner.resolve(name).to_owned();
        let Some(Type::Enum(id)) = expected else {
            // Still analyze any arguments so their own mistakes are reported.
            if let Some(args) = args {
                for &arg in args {
                    self.analyze_expr(ctx, arg);
                }
            }
            let message = match expected {
                Some(ty) if ty != Type::Error => format!(
                    "a leading-dot member is an enum variant, but `{}` is expected here",
                    self.type_name(ty)
                ),
                _ => "a leading-dot member needs a known enum type here".to_owned(),
            };
            if expected != Some(Type::Error) {
                self.emit(span, "KSEM119", message);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };

        let Some(tag) = self
            .program
            .types
            .enums()
            .get(id)
            .and_then(|def| def.variant_index(&member))
        else {
            if let Some(args) = args {
                for &arg in args {
                    self.analyze_expr(ctx, arg);
                }
            }
            self.emit(
                name_span,
                "KSEM120",
                format!(
                    "enum `{}` has no variant `{member}`",
                    self.type_name(Type::Enum(id))
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };

        if let Some(owner) = self
            .program
            .types
            .enums()
            .get(id)
            .map(|def| def.name.clone())
        {
            self.link_variant_name(&owner, &member, name_span);
        }
        let payload = self.analyze_variant_payload(ctx, id, tag, &member, args, span);
        self.program.exprs.alloc(HirExpr::EnumNew {
            enum_id: id,
            tag,
            payload,
        })
    }

    /// Resolves a variant's payload: the written argument, or the declared
    /// default when none is written.
    fn analyze_variant_payload(
        &mut self,
        ctx: &mut FnCtx,
        id: EnumId,
        tag: u32,
        member: &str,
        args: &Option<Vec<ExprId>>,
        span: Span,
    ) -> Option<HirExprId> {
        let payload_ty = self
            .program
            .types
            .enums()
            .get(id)
            .and_then(|def| def.variant(tag))
            .and_then(|variant| variant.payload);
        let written: &[ExprId] = args.as_deref().unwrap_or(&[]);
        match payload_ty {
            None => {
                // A payload-less variant takes no argument.
                if !written.is_empty() {
                    for &arg in written {
                        self.analyze_expr(ctx, arg);
                    }
                    self.emit(
                        span,
                        "KSEM121",
                        format!("variant `{member}` takes no payload"),
                    );
                }
                None
            }
            Some(expected) => {
                if written.len() > 1 {
                    for &arg in written {
                        self.analyze_expr(ctx, arg);
                    }
                    self.emit(
                        span,
                        "KSEM122",
                        format!("variant `{member}` takes exactly one payload value"),
                    );
                    return Some(self.program.exprs.alloc(HirExpr::Error));
                }
                if let Some(&arg) = written.first() {
                    let value = self.analyze_expr_expecting(ctx, arg, Some(expected));
                    let value_ty = self.program.expr(value).type_of();
                    if !value_ty.assignable_to(expected) {
                        self.emit(
                            self.tree.expr(arg).span(),
                            "KSEM123",
                            format!(
                                "variant `{member}` expects a payload of `{}`, found `{}`",
                                self.type_name(expected),
                                self.type_name(value_ty)
                            ),
                        );
                    }
                    return Some(value);
                }
                // No argument written: fall back to the declared default.
                match self.variant_default(id, tag) {
                    Some(default) => Some(self.analyze_default(default, Some(expected))),
                    None => {
                        self.emit(
                            span,
                            "KSEM124",
                            format!(
                                "variant `{member}` requires a payload value (no default is \
                                 declared)"
                            ),
                        );
                        Some(self.program.exprs.alloc(HirExpr::Error))
                    }
                }
            }
        }
    }

    /// Builds the tag operand for one side of an enum equality.
    ///
    /// A payload-less variant literal folds straight to its tag constant, so
    /// `c == .Red` compares `EnumTag(c)` against `Int(red)` with no throwaway
    /// enum. Anything else reads its tag at run time with [`HirExpr::EnumTag`].
    pub(crate) fn enum_tag_operand(&mut self, operand: HirExprId) -> HirExprId {
        if let HirExpr::EnumNew {
            tag, payload: None, ..
        } = self.program.expr(operand)
        {
            let tag = i64::from(*tag);
            return self.program.exprs.alloc(HirExpr::Int(tag));
        }
        self.program
            .exprs
            .alloc(HirExpr::EnumTag { value: operand })
    }

    /// Builds `lhs == rhs` / `lhs != rhs` for two enum values as a tag
    /// comparison, given they share an enum type.
    pub(crate) fn enum_equality(
        &mut self,
        is_eq: bool,
        lhs: HirExprId,
        rhs: HirExprId,
    ) -> HirExprId {
        let lhs_tag = self.enum_tag_operand(lhs);
        let rhs_tag = self.enum_tag_operand(rhs);
        let op = if is_eq {
            HirBinaryOp::EqInt
        } else {
            HirBinaryOp::NeInt
        };
        self.program.exprs.alloc(HirExpr::Binary {
            op,
            lhs: lhs_tag,
            rhs: rhs_tag,
            ty: Type::Bool,
        })
    }
}
