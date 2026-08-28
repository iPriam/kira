//! Calls whose receiver is a *name* rather than a value.
//!
//! `Root.name(args)` parses as a method call because the parser cannot tell a
//! name that stands for a value from one that stands for a module, a parent
//! type, or a declaration. Only the analyzer — holding the import table, the
//! class table, and the construct tables — can, so the three name-rooted forms
//! are separated here before anything analyzes `Root` as an expression:
//!
//! - `Support.hello()` — a module-qualified free call.
//! - `ClsAccount.gross()` — the parent's body run against `self`, this
//!   language's spelling of "super".
//! - `Sprite.draw()` — a construct-backed *declaration*, which names a thing
//!   rather than a type and is therefore callable without one being built.
//!
//! Split out of [`super::calls`] on the file-size ladder. They belong together
//! for a reason beyond size: each is decided by what `Root` resolves to, and
//! the order they are tried in is the whole rule — a nearer meaning wins, and
//! once one claims the call none of the others may report on it.

use kira_semantics_model::StructId;
use kira_semantics_model::Type;
use kira_semantics_model::hir::{HirExpr, HirExprId};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, Expr, ExprId};

use crate::analyze::{Analyzer, FnCtx};

/// The syntax and receiver context for a construct-backed declaration call.
struct ConstructDeclarationCall<'a> {
    /// The function context receiving ownership effects.
    ctx: &'a mut FnCtx,
    /// The declaration being called.
    id: StructId,
    /// The source expression naming the declaration.
    receiver_syntax: ExprId,
    /// The member name.
    method: &'a str,
    /// The written member arguments.
    args: &'a [CallArg],
    /// The enclosing call span.
    root_span: Span,
    /// The member span.
    method_span: Span,
}

impl Analyzer<'_> {
    /// Resolves `Root.name(args)` when `Root` is a name rather than a value.
    ///
    /// Returns `None` when the shape is not a name-rooted call at all, so the
    /// caller carries on as a method call. It returns `Some(Error)` — not
    /// `None` — when `Root` is a module the *program* has but this file did
    /// not import: that is a real mistake with a real diagnostic, and falling
    /// through to "type `Error` has no methods" would bury it.
    pub(super) fn analyze_qualified_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: ExprId,
        method: kira_core::Symbol,
        method_span: Span,
        args: &[CallArg],
    ) -> Option<HirExprId> {
        let Expr::Name { symbol, span } = *self.tree.expr(receiver) else {
            return None;
        };
        let root = self.interner.resolve(symbol).to_owned();
        // A local of the same name wins: a binding the reader can see beats a
        // module they have to look up, and it is what keeps a module name from
        // becoming unusable as a variable.
        if ctx.resolve(&root).is_some() {
            return None;
        }
        // `ClsAccount.gross()` inside a subclass method: the parent's body run
        // against `self`. Checked before the module table, because a parent
        // type name is nearer than a module name.
        if self.visible_struct(&root).is_some() {
            let name = self.interner.resolve(method).to_owned();
            // A construct-backed declaration names a *declaration*, not a type,
            // so it answers a call on its own terms rather than as a parent
            // qualifier. It is checked first because it is the nearer meaning:
            // `Sprite.draw()` is about `Sprite`, and only a class hierarchy
            // makes a bare type name mean "run the inherited body".
            if let Some(id) = self.construct_backed_named(&root) {
                return Some(
                    self.analyze_construct_declaration_call(ConstructDeclarationCall {
                        ctx,
                        id,
                        receiver_syntax: receiver,
                        method: &name,
                        args,
                        root_span: span,
                        method_span,
                    }),
                );
            }
            // Once the root is known to be a type name, this path owns the
            // call: falling through would analyze the type name as a value and
            // report it undefined on top of whatever was already said.
            let Some(qualifier) = self.resolve_parent_qualifier(ctx, &root, span) else {
                for arg in args {
                    self.analyze_expr(ctx, arg.value);
                }
                return Some(self.program.exprs.alloc(HirExpr::Error));
            };
            return Some(self.analyze_parent_call(ctx, qualifier, &name, args, method_span));
        }
        if self.module_for_root(&root).is_some() {
            let name = self.interner.resolve(method).to_owned();
            return Some(self.analyze_user_call_from_syntax(ctx, &name, &[], args, method_span));
        }
        if self.report_unimported_root(&root, span) {
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        None
    }

    /// Type-checks `Decl.member(args)`, where `Decl` is a construct-backed
    /// declaration.
    ///
    /// A declaration is a named thing with a default for every input, so naming
    /// it is enough to call a member on it — no `Decl()` at the call site. Which
    /// value the member runs against depends on where the call is written:
    ///
    /// - Inside one of `Decl`'s own members, the qualifier is `self`. A member
    ///   calling a sibling this way must see the values this instance was built
    ///   with, not a second default one.
    /// - Anywhere else, it is `Decl()` — the default construction, reported
    ///   exactly as a written `Decl()` would be when an input has no default.
    ///
    /// A plain `struct` or `class` name gets none of this: it names a type, and
    /// a type has no values of its own to run a member against.
    fn analyze_construct_declaration_call(
        &mut self,
        call: ConstructDeclarationCall<'_>,
    ) -> HirExprId {
        let ConstructDeclarationCall {
            ctx,
            id,
            receiver_syntax,
            method,
            args,
            root_span,
            method_span,
        } = call;
        let own_member = ctx.receiver == Some(id);
        let receiver_hir = match own_member.then(|| ctx.resolve("self")).flatten() {
            Some(local) => self.program.exprs.alloc(HirExpr::Local {
                local,
                ty: Type::Struct(id),
            }),
            None => self.analyze_construct_new(ctx, id, &[], &[], root_span),
        };
        let owner = self.program.types.type_name(Type::Struct(id));
        let qualified = format!("{owner}.{method}");
        if self.lookup_function(&qualified).is_none() {
            // A declaration also answers its family's uniform `extend`
            // modifiers, the same way a constructed value does: upcast into the
            // family and dispatch there.
            if let Some(family_id) = self.family_uniform_method(id, method) {
                let upcast = self.coerce_construct_value(receiver_hir, Some(Type::Enum(family_id)));
                let call = self.analyze_construct_family_call(
                    ctx,
                    upcast,
                    family_id,
                    method,
                    crate::constructs::ConstructCallContent {
                        args,
                        children: &[],
                        receiver_syntax: (!own_member).then_some(receiver_syntax),
                    },
                    method_span,
                );
                return call;
            }
            self.emit(
                method_span,
                "KSEM097",
                format!("`{owner}` has no member `{method}`"),
            );
            for arg in args {
                self.analyze_expr(ctx, arg.value);
            }
            return self.program.exprs.alloc(HirExpr::Error);
        }
        let call =
            self.analyze_user_call_from_syntax(ctx, &qualified, &[receiver_hir], args, method_span);
        // Running against `self` means a mutating member writes this instance
        // back, exactly as a parent-qualified call does.
        if own_member && let Some(local) = ctx.resolve("self") {
            self.record_mut_self(call, local);
        }
        call
    }
}
