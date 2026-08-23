//! Choosing how a construct-backed declaration is constructed.
//!
//! A declaration has one **primary** way to be built — its parenthesized header,
//! which fills its stored members directly — and any number of secondary
//! `init(…)` members, each another way, told apart by what it takes:
//!
//! ```text
//! Widget NavigationLink(destination: Any Widget, label: some Widget) {
//!     init(title: String, value: Any) {
//!         return NavigationLink(destination: Destination(value), label: Text(title))
//!     }
//! }
//! ```
//!
//! An `init` is a function returning the declaration, so its body ends in a
//! construction of the same declaration through the primary form. Nothing new
//! reaches lowering: the primary still produces the `StructNew` it always did,
//! and an init is an ordinary call of an ordinary function.
//!
//! `NavigationLink(…)` therefore names an overload set of the primary plus every
//! init, and the call picks among them by [the same rule every other overloaded
//! name uses](crate::typeck::overloads).

use kira_semantics_model::hir::{FuncId, HirExpr, HirExprId, HirStmt};
use kira_semantics_model::{StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{CallArg, ExprId, TypeRef};

use super::ContentSlot;
use crate::analyze::{Analyzer, Callable, FnCtx, InitContent};

/// How one construction site was resolved.
enum Construction {
    /// The parenthesized header: fill the stored members directly.
    Primary,
    /// One `init(…)`: call it and let its body construct.
    Init(FuncId),
}

impl<'a> Analyzer<'a> {
    /// Records which parameter of each `init(…)` its construction's trailing
    /// children fill.
    ///
    /// A parameter written `some X` / `[some X]` is a content parameter, the
    /// same spelling that marks a declaration's child slot. It must be the last
    /// one: the children are written after every parenthesized argument, so a
    /// content parameter anywhere else would leave the block filling a slot with
    /// written arguments after it.
    pub(crate) fn record_init_content(&mut self, callables: &[Callable<'a>]) {
        for (index, callable) in callables.iter().enumerate() {
            // Content is not an `init` idea. Any callable whose LAST parameter
            // is `some X` — a method, a modifier, a free function — takes its
            // children from a trailing block, because the children are written
            // after every parenthesized argument and so fill the slot that
            // follows them. Only the "not last" mistake stays init-specific,
            // because only a construction can currently write children before
            // an argument and be sure it meant to.
            let owner = callable.initializes;
            self.source = callable.source;
            let params = &callable.function.params;
            let mut content: Option<InitContent> = None;
            for (slot, param) in params.iter().enumerate() {
                let (element_ref, list) = match self.tree.type_ref(param.ty) {
                    TypeRef::SomeConstruct { .. } => (param.ty, false),
                    TypeRef::Array { element, .. }
                        if matches!(
                            self.tree.type_ref(*element),
                            TypeRef::SomeConstruct { .. }
                        ) =>
                    {
                        (*element, true)
                    }
                    _ => continue,
                };
                if content.is_some() || slot + 1 != params.len() {
                    if owner.is_none() {
                        continue;
                    }
                    self.emit(
                        param.name_span,
                        "KSEM276",
                        "an `init` takes content in its last parameter only: the children are \
                         written after every argument, so one `some X` parameter can hold them",
                    );
                    content = None;
                    break;
                }
                content = Some(InitContent {
                    slot,
                    list,
                    element: self.resolve_type_ref(element_ref),
                });
            }
            if let Some(content) = content {
                self.init_content.insert(index as u32, content);
            }
            if let Some(owner) = owner {
                self.refuse_init_shadowing_the_header(
                    FuncId(index as u32),
                    owner,
                    callable,
                    content,
                );
            }
        }
    }

    /// Reports an `init` that takes exactly what the parenthesized header
    /// takes.
    ///
    /// Two ways to construct one declaration must differ in what they take, for
    /// the reason two overloads of a name must: otherwise a construction fits
    /// both and nothing in the source says which one it meant.
    fn refuse_init_shadowing_the_header(
        &mut self,
        init: FuncId,
        owner: StructId,
        callable: &Callable<'_>,
        content: Option<InitContent>,
    ) {
        let header: Vec<Type> = self
            .construct_input_slots(owner)
            .into_iter()
            .map(|input| input.ty)
            .collect();
        // A content parameter is reached by a trailing block rather than by a
        // written argument, so it is compared against the header's own child
        // slot: an init that takes content collides only with a header that
        // takes content too.
        let mut written = self.param_types(init);
        if content.is_some() {
            written.pop();
            if self.child_slots(owner).is_empty() {
                return;
            }
        }
        if written != header {
            return;
        }
        let name = self.program.types.type_name(Type::Struct(owner));
        self.emit(
            callable.function.name_span,
            "KSEM278",
            format!(
                "this `init` takes what `{name}` already takes between its parentheses, so a \
                 construction fits both"
            ),
        );
    }

    /// The content parameter of `init`, when it declared one.
    pub(crate) fn init_content_param(&self, init: FuncId) -> Option<InitContent> {
        self.init_content.get(&init.0).copied()
    }

    /// Type-checks `Name(args) { children }` for a construct-backed
    /// declaration, through whichever way of constructing it fits.
    pub(crate) fn analyze_construction(
        &mut self,
        ctx: &mut FnCtx,
        id: StructId,
        args: &[CallArg],
        children: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let name = self.initializer_name(id);
        let inits = self.visible_overloads(&name);
        if inits.is_empty() {
            return self.analyze_construct_new(ctx, id, args, children, span);
        }
        match self.pick_construction(ctx, id, &inits, args, children) {
            Construction::Primary => self.analyze_construct_new(ctx, id, args, children, span),
            Construction::Init(chosen) => {
                self.link_function(chosen, span);
                // The trailing children are the content parameter's value, so
                // they are built here and handed to the call as an argument
                // already analyzed — there is no written expression for them.
                let trailing = match self.init_content_param(chosen) {
                    Some(content) if !children.is_empty() => {
                        vec![self.content_value(ctx, &content, children, span)]
                    }
                    _ => Vec::new(),
                };
                self.analyze_user_call_from_syntax_with(ctx, &name, &[], args, &trailing, span)
            }
        }
    }

    /// Builds the value a content parameter takes from a construction's
    /// trailing children.
    pub(crate) fn content_value(
        &mut self,
        ctx: &mut FnCtx,
        content: &InitContent,
        children: &[ExprId],
        span: Span,
    ) -> HirExprId {
        if content.list {
            let ty = self.program.types.array_of(content.element);
            // A `For`/`if` builder among the children builds the list at run
            // time, exactly as one in a construction's block does. Sharing the
            // expansion rather than repeating it is what keeps the two blocks
            // the same language: a builder that works inside `HStack { … }` has
            // no reason to stop working inside `.toolbar { … }`.
            if children.iter().any(|&child| self.is_builder_item(child)) {
                let slot = ContentSlot {
                    field_index: 0,
                    name: "content".to_owned(),
                    list: true,
                    element_ty: content.element,
                    field_ty: ty,
                    has_default: false,
                };
                let acc = ctx.declare_hidden(ty, true);
                let empty = self.program.exprs.alloc(HirExpr::ArrayNew {
                    ty,
                    elements: Vec::new(),
                });
                let mut stmts = vec![self.program.stmts.alloc(HirStmt::Let {
                    local: acc,
                    init: empty,
                })];
                self.expand_content_items(ctx, children, acc, &slot, "content", &mut stmts);
                for stmt in stmts {
                    ctx.hoist_stmt(stmt);
                }
                return self.program.exprs.alloc(HirExpr::Local { local: acc, ty });
            }
            let elements = children
                .iter()
                .map(|&child| self.analyze_expr_expecting(ctx, child, Some(content.element)))
                .collect();
            return self.program.exprs.alloc(HirExpr::ArrayNew { ty, elements });
        }
        let [only] = children else {
            for &child in children {
                self.analyze_expr(ctx, child);
            }
            self.emit(
                span,
                "KSEM277",
                format!(
                    "this `init` takes exactly one child, found {}; a `[some X]` parameter takes \
                     a list of them",
                    children.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        };
        self.analyze_expr_expecting(ctx, *only, Some(content.element))
    }

    /// Which way of constructing `id` these arguments mean.
    ///
    /// Both kinds are scored on one scale, so `NavigationLink(destination:,
    /// label:)` reaches the header and `NavigationLink(title:, value:)` reaches
    /// the init, without either being tried first and falling through.
    fn pick_construction(
        &mut self,
        ctx: &FnCtx,
        id: StructId,
        inits: &[FuncId],
        args: &[CallArg],
        children: &[ExprId],
    ) -> Construction {
        let actual = self.try_argument_types(ctx, &[], args);
        // Trailing content fills the header's own child slots; a declaration
        // that has none cannot be built that way.
        let primary = match children.is_empty() || !self.child_slots(id).is_empty() {
            true => self.primary_score(id, args, &actual),
            false => None,
        };
        let best_init = inits
            .iter()
            .filter_map(|&init| {
                let mut types = actual.clone();
                if !children.is_empty() {
                    // The block fills the content parameter, which follows every
                    // written argument. An init with none, or with written
                    // arguments still to come, is not what this construction
                    // means.
                    let content = self.init_content_param(init)?;
                    if content.slot != types.len() {
                        return None;
                    }
                    types.push(content.element);
                }
                Some((init, self.overload_score(init, &types)?))
            })
            .min_by_key(|(_, score)| *score);
        match (primary, best_init) {
            (Some(header), Some((_, init))) if header <= init => Construction::Primary,
            (_, Some((init, _))) => Construction::Init(init),
            // Nothing fits. The header carries the diagnostic, because it is
            // what the declaration says it takes.
            (_, None) => Construction::Primary,
        }
    }

    /// How badly `args` fit the declaration's parenthesized header, or `None`
    /// when they do not fit at all.
    ///
    /// Scored the way an overload is, with one difference the header alone has:
    /// a labeled argument binds to the *input of that name* rather than to its
    /// written position, because a construction input is a member.
    fn primary_score(
        &mut self,
        id: StructId,
        args: &[CallArg],
        actual: &[Type],
    ) -> Option<(u32, u32)> {
        let inputs = self.construct_input_slots(id);
        let mut filled: Vec<Option<Type>> = vec![None; inputs.len()];
        let mut next_positional = 0usize;
        for (arg, &ty) in args.iter().zip(actual.iter()) {
            let slot = match arg.label {
                Some(label) => {
                    let label = self.interner.resolve(label).to_owned();
                    inputs.iter().position(|input| input.name == label)?
                }
                None => {
                    let slot = next_positional;
                    next_positional += 1;
                    slot
                }
            };
            if slot >= filled.len() || filled[slot].is_some() {
                return None;
            }
            filled[slot] = Some(ty);
        }
        let mut conversions = 0;
        let mut defaulted = 0;
        for (slot, input) in inputs.iter().enumerate() {
            match filled[slot] {
                Some(ty) => {
                    if !self.argument_reaches(ty, input.ty) {
                        return None;
                    }
                    if ty != input.ty {
                        conversions += 1;
                    }
                }
                // A slot nobody filled takes the member's declared default; one
                // with no default is a construction the header cannot serve.
                None => {
                    self.field_default(id, input.field_index)?;
                    defaulted += 1;
                }
            }
        }
        Some((conversions, defaulted))
    }
}
