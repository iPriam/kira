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

/// What the base of a qualified member spelling (`SizeMode.Hug`, `Result.Ok`)
/// turned out to name.
///
/// Three outcomes rather than an `Option`, because a generic enum written
/// without an instantiation to construct is neither a resolved enum nor an
/// ordinary value: the caller owes it a diagnostic instead of falling through
/// to "undefined name".
#[derive(Debug, Clone)]
pub(crate) enum QualifiedEnum {
    /// The base names this enum, and the member after it is a variant.
    Enum(EnumId),
    /// The base names this generic enum, and nothing here says which
    /// instantiation of it is meant.
    Unanchored(String),
    /// The base names no enum; it is an ordinary expression.
    NotAnEnum,
}

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
            // Keyed by the declaring package, exactly as a struct is: two
            // packages may each declare a `Color`, and neither is a duplicate.
            match self.program.types.enums_mut().declare_owned(
                self.imports.package_of(source),
                EnumDef {
                    name: name.clone(),
                    variants: Vec::new(),
                },
            ) {
                Some(id) => {
                    // Reserved in id order now and filled by the second pass, so
                    // `enum_defaults` stays indexed by the ids the table minted.
                    self.enum_defaults.push(Vec::new());
                    self.enum_sources.insert(id, source);
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

    /// Reports every enum that has no finite value, and breaks the one it
    /// reports so that later passes can build one.
    ///
    /// An enum whose every variant carries a payload leading back into the enum
    /// has no value at all: writing one would need a value of itself first.
    /// `KSEM052` catches the struct-only spelling of that, and the escape it
    /// recommends is an enum — so this is where the escape's own failure is
    /// caught, at the declaration, rather than in whichever later pass first
    /// tries to build a value of the type.
    ///
    /// Runs after every body, because a body is what mints a generic
    /// instantiation, and before the closure desugar, which is the first pass to
    /// ask for a value of a type nobody wrote.
    ///
    /// A reported enum keeps its variants and loses their payloads, exactly as a
    /// broken struct field becomes `Error`: the program is already rejected, and
    /// what every later walk needs is a shape it can finish.
    pub(crate) fn check_enum_terminates(&mut self) {
        // Walked by id rather than by name: names repeat across packages by
        // design now, and a name-only lookup would silently skip every
        // package-owned row.
        let declared: Vec<(EnumId, String)> = self
            .program
            .types
            .enums()
            .ids()
            .filter_map(|id| {
                self.program
                    .types
                    .enums()
                    .get(id)
                    .map(|def| (id, def.name.clone()))
            })
            .collect();
        for (id, name) in declared {
            // An enum with no variants at all is uninhabited by declaration
            // rather than by mistake, and a construct family that no declaration
            // backs yet is exactly that shape. Nothing can write a value of one,
            // so there is nothing to warn the author about.
            if self
                .program
                .types
                .enums()
                .get(id)
                .is_none_or(|def| def.variants.is_empty())
            {
                continue;
            }
            if self.has_finite_value(Type::Enum(id)) {
                continue;
            }
            let Some(broken) = self.program.types.enums().get(id).map(|def| {
                def.variants
                    .iter()
                    .map(|variant| VariantDef {
                        name: variant.name.clone(),
                        payload: variant.payload.map(|_| Type::Error),
                    })
                    .collect()
            }) else {
                continue;
            };
            self.program.types.enums_mut().set_variants(id, broken);
            let span = match self.enum_declaration_site(id, &name) {
                Some((source, span)) => {
                    self.source = source;
                    span
                }
                None => Span::new(0, 0),
            };
            self.emit(
                span,
                "KSEM272",
                format!(
                    "enum `{name}` has no variant with a finite value: every variant carries a \
                     payload that leads back into `{name}`, so no value of it can ever be built. \
                     Give it a variant with no payload, or hold the recursive payload behind an \
                     array (`[{name}]`)."
                ),
            );
        }
    }

    /// The file and span of the `enum` declaration written under `name`.
    ///
    /// A generic instantiation is named for its template and its arguments
    /// (`Result<Int, AppError>`), and the only line there is to point at is the
    /// template's, so the lookup is by the name before the arguments.
    /// The file and span of the `enum` declaration written under `name`.
    ///
    /// Matched against the row's owner as well as the name: names repeat
    /// across packages, so the first tree-order hit by name alone could be the
    /// other package's declaration.
    fn enum_declaration_site(&self, id: EnumId, name: &str) -> Option<(SourceId, Span)> {
        let written = name.split_once('<').map_or(name, |(base, _)| base);
        let owner = self.program.types.enums().owner_of(id);
        self.tree
            .items_with_source()
            .find_map(|(source, item)| match item {
                Item::Enum(declaration)
                    if self.interner.resolve(declaration.name) == written
                        && self.imports.package_of(source) == owner =>
                {
                    Some((source, declaration.name_span))
                }
                _ => None,
            })
    }

    /// Restricts an enum payload to a type the runtime box can carry.
    ///
    /// The box holds one type-erased value slot. A scalar or pointer word fits
    /// directly; a `String`, nested enum, or capture cell is an owned handle; a
    /// struct or an array uses the erased aggregate box, whose compiler-generated
    /// clone/free leaves carry the element callbacks the payload word cannot.
    ///
    /// A nested enum is admitted because `Result`-shaped values are built from
    /// one: `Error` carries the failure enum, which is what
    /// `attempt`/`try`/`handle` routes on. Every layer already reclaims it
    /// recursively — the VM's `Heap::copy_value`/`free_enum`, the native box's
    /// `EnumPayloadKind::ENUM`, and the WASM lowering's handle payload — and the
    /// recursion terminates because a payload's type resolves against types that
    /// already resolve, so a cycle is unrepresentable.
    ///
    /// What is left out is not short of room in the box — it is a type no
    /// declaration may name a payload of at all: `Void`, a `CString` view the
    /// payload would not own, an in-flight `Task`, and a `NativeState` handle
    /// whose lifetime is the host's.
    fn check_payload_type(&mut self, ty: Type, span: Span) -> Type {
        // Whether a payload runs a user `Drop` cannot be asked here: a `Drop`
        // conformance is collected after every payload is resolved. The site is
        // recorded instead, with the span this pass already worked out — an
        // instantiation blames the use site — and
        // [`Analyzer::refuse_drop_enum_payloads`] answers once the conformances
        // exist.
        self.enum_payload_sites.push((ty, self.source, span));
        match ty {
            Type::Int(_)
            | Type::Float(_)
            | Type::Bool
            | Type::String
            | Type::RawPtr
            | Type::ForeignPtr(_)
            | Type::Struct(_)
            | Type::Enum(_)
            // A struct and an array both travel as an aggregate: the box owns a
            // copy plus the two leaves that clone and free it, so an element
            // type needing its own teardown (a `[String]`, a `[[Int]]`) is
            // reclaimed by the generated leaf rather than by the box's kind tag.
            | Type::Array(_)
            // An erased value is an owned handle to a box shaped exactly like a
            // nested enum's, so it travels on the arm above's terms and needs
            // nothing of its own: `EnumPayloadKind::ENUM` on native reclaims it,
            // and the VM's `Value` was never told the difference.
            | Type::Any
            // A cell is a retained shared handle. The enum owns one share and
            // releases it with the rest of its payload, while the captured
            // binding and any other closure keep their own shares.
            | Type::Cell(_)
            | Type::Error => ty,
            _ => {
                self.emit(
                    span,
                    "KSEM118",
                    format!(
                        "an enum payload may not be of type `{}`; a payload may be \
                         `Int`, `Float`, `Bool`, `String`, `Any`, a pointer, an array, a \
                         struct, or another enum",
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
    /// qualifier here.
    ///
    /// A generic enum resolves too, but only through `expected`: `Result.Ok(1)`
    /// spells no type arguments, so the position has to supply them. Written
    /// where nothing asks for an instantiation of that template, it is
    /// [`QualifiedEnum::Unanchored`] — a mistake with its own fix, not an
    /// undefined name.
    pub(crate) fn qualified_enum_at(
        &self,
        ctx: &FnCtx,
        base: ExprId,
        expected: Option<Type>,
    ) -> QualifiedEnum {
        let Some(path) = self.name_path_of(base) else {
            return QualifiedEnum::NotAnEnum;
        };
        let candidate = match path.split_once('.') {
            None => {
                if ctx.resolve(&path).is_some() {
                    return QualifiedEnum::NotAnEnum;
                }
                path
            }
            Some((root, member)) => {
                // A module-qualified enum: strip the imported root. A local
                // named like the root wins, and a further-dotted member is not
                // a single enum name.
                if ctx.resolve(root).is_some()
                    || member.contains('.')
                    || self.module_for_root(root).is_none()
                {
                    return QualifiedEnum::NotAnEnum;
                }
                member.to_owned()
            }
        };
        if self.is_generic_enum(&candidate) {
            return match self.generic_instantiation_expected(&candidate, expected) {
                Some(id) => QualifiedEnum::Enum(id),
                None => QualifiedEnum::Unanchored(candidate),
            };
        }
        // Resolved the way a written type name is: owner-keyed, because names
        // repeat across packages by design and a bare name means this file's
        // own package's first.
        match self.visible_enum(&candidate) {
            Some(id) => QualifiedEnum::Enum(id),
            None => QualifiedEnum::NotAnEnum,
        }
    }

    /// Reports a generic enum constructed with no instantiation to construct.
    ///
    /// The fix is never to add type arguments to the constructor — the language
    /// has no `Result<Int, Bool>.Ok(1)` — it is to give the position a type, so
    /// the message says that rather than repeating the name.
    pub(crate) fn report_unanchored_generic_construction(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        member: kira_core::Symbol,
        args: &Option<Vec<ExprId>>,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        // Still analyze any arguments so their own mistakes are reported.
        if let Some(args) = args {
            for &arg in args {
                self.analyze_expr(ctx, arg);
            }
        }
        // An expectation that already failed to resolve said its piece; naming
        // a second mistake on top of it would blame the wrong line.
        if expected != Some(Type::Error) {
            let variant = self.interner.resolve(member).to_owned();
            let detail = match expected {
                Some(ty) => format!(
                    "but `{}` is expected here, which is not one of its instantiations",
                    self.type_name(ty)
                ),
                None => "but nothing here says which instantiation is meant".to_owned(),
            };
            self.emit(
                span,
                "KSEM254",
                format!(
                    "generic enum `{name}` needs an instantiation to construct, {detail}; \
                     annotate the target, as in \
                     `let value: {name}<...> = {name}.{variant}(...)`"
                ),
            );
        }
        self.program.exprs.alloc(HirExpr::Error)
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
                    if !self.admits(value_ty, expected) {
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
                    return Some(self.coerce_into(value, expected));
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
