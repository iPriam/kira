//! Minting function types, lifting closure literals, and capturing.

use kira_semantics_model::hir::{FuncId, HirExpr, HirExprId, HirFunction, HirStmt, LocalId};
use kira_semantics_model::{FieldDef, OwnershipMode, StructDef, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::{Block, ClosureParam};

use super::{Capture, Captured, ClosureCtx, ClosureImpl, ClosureSite, FnTypeInfo};
use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// The type of `(params) -> result`, minting its representation struct on
    /// first mention.
    ///
    /// Idempotent by shape, so `(Int) -> Void` written in two files is one
    /// type and a closure made for one fits the other.
    ///
    /// The parameter *modes* are part of that shape. `(borrow Event) -> Void`
    /// and `(Event) -> Void` are two types, because a call through the first
    /// leaves the caller's value alone and a call through the second takes it —
    /// so a value of one is not a value of the other.
    pub(crate) fn function_type(
        &mut self,
        params: Vec<Type>,
        param_ownership: Vec<OwnershipMode>,
        result: Type,
    ) -> Type {
        // An unresolvable part makes the whole type unresolvable, exactly as it
        // does for an array element: interning `(<error>) -> Int` would mint a
        // row a second bad type would compare *equal* to.
        if params.contains(&Type::Error) || result == Type::Error {
            return Type::Error;
        }
        let key = (params.clone(), param_ownership.clone(), result);
        if let Some(id) = self.fn_types.lookup(&key) {
            return Type::Struct(id);
        }
        let name = self.function_type_name(&params, &param_ownership, result);
        // The name carries parentheses and an arrow, so it can collide with no
        // declared struct: an identifier holds neither.
        let Some(id) = self.program.types.structs_mut().declare(StructDef {
            name,
            fields: vec![FieldDef {
                name: "tag".to_owned(),
                ty: Type::INT,
                mutable: false,
            }],
        }) else {
            return Type::Error;
        };
        // Kept in step with the table: `struct_defaults` is indexed by the ids
        // the table mints, and this mints one outside `collect_structs`.
        self.struct_defaults.push(vec![None]);
        self.fn_types.insert(
            key,
            id,
            FnTypeInfo {
                params,
                param_ownership,
                result,
                dispatcher: None,
                impls: Vec::new(),
                named_functions: std::collections::HashMap::new(),
            },
        );
        Type::Struct(id)
    }

    /// The canonical spelling of a function type, which is also the name its
    /// representation struct is declared under — so every diagnostic that
    /// names a type names this one as it was written.
    ///
    /// The modes are spelled too, because they are part of the type: two
    /// function types that differ only in a mode must not print the same, or a
    /// mismatch between them reads as a type not matching itself.
    pub(crate) fn function_type_name(
        &self,
        params: &[Type],
        param_ownership: &[OwnershipMode],
        result: Type,
    ) -> String {
        let written: Vec<String> = params
            .iter()
            .enumerate()
            .map(|(index, &ty)| {
                let name = self.type_name(ty);
                match param_ownership.get(index) {
                    Some(OwnershipMode::Owned) | None => name,
                    Some(mode) => format!("{} {name}", mode.spelling()),
                }
            })
            .collect();
        format!("({}) -> {}", written.join(", "), self.type_name(result))
    }

    /// Reserves the next synthesized function id.
    ///
    /// Synthesized functions sit after every declared one, so an id is the
    /// declared count plus the number reserved so far. Reserving and filling
    /// are separate because a dispatcher's id is needed at the call site, long
    /// before its body can be built.
    pub(crate) fn reserve_synth(&mut self) -> FuncId {
        let id = FuncId(self.synth_base + self.synth.len() as u32);
        self.synth.push(None);
        id
    }

    /// Records a synthesized function's body against its reserved id.
    pub(crate) fn fill_synth(&mut self, id: FuncId, function: HirFunction) {
        let index = (id.0 - self.synth_base) as usize;
        if let Some(slot) = self.synth.get_mut(index) {
            *slot = Some(function);
        }
    }

    /// Type-checks a closure literal against the function type expected here.
    ///
    /// A closure never writes its parameter types, so without an expected type
    /// there is nothing to check the body against and nothing to build a value
    /// of — which is why an unexpected closure is an error rather than a
    /// guess.
    pub(crate) fn analyze_closure(
        &mut self,
        ctx: &mut FnCtx,
        params: &[ClosureParam],
        body: &Block,
        span: Span,
        expected: Option<Type>,
    ) -> HirExprId {
        let Some(repr) = expected.and_then(|ty| match ty {
            Type::Struct(id) if self.fn_types.get(id).is_some() => Some(id),
            _ => None,
        }) else {
            // An `Error` expectation already had its say.
            if expected != Some(Type::Error) {
                let found = match expected {
                    Some(ty) => format!("`{}` is expected here", self.type_name(ty)),
                    None => "nothing here says what its parameters are".to_owned(),
                };
                self.emit(
                    span,
                    "KSEM134",
                    format!("a closure needs a function type to check against, but {found}"),
                );
            }
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let info = self
            .fn_types
            .get(repr)
            .map(|info| (info.params.clone(), info.result));
        let Some((param_types, result)) = info else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        if params.len() != param_types.len() {
            self.emit(
                span,
                "KSEM135",
                format!(
                    "`{}` takes {} parameter(s), but this closure declares {}",
                    self.type_name(Type::Struct(repr)),
                    param_types.len(),
                    params.len()
                ),
            );
            return self.program.exprs.alloc(HirExpr::Error);
        }
        self.lift_closure(ctx, repr, &param_types, result, params, body)
    }

    /// Resolves a bare function name as a first-class function value.
    ///
    /// A local or field of the same name has already won before this is called.
    /// With an expected function type, the named function must match it exactly;
    /// without one, its signature supplies the function value's type.
    pub(crate) fn analyze_named_function_reference(
        &mut self,
        name: &str,
        span: Span,
        expected: Option<Type>,
    ) -> Option<HirExprId> {
        let (target, params, result) = self
            .lookup_function(name)
            .map(|(id, params, result)| (id, params.to_vec(), result))?;
        // A named function's own parameter modes are part of the type it has,
        // so they take part in the match: `graphicsApplicationDefaultEvent`
        // declares `borrow`, and it fits a `(borrow GraphicsEvent) -> Void`
        // slot rather than an owned one.
        let modes = self.param_ownership(target);
        let repr = match expected {
            Some(Type::Struct(id)) if self.fn_types.get(id).is_some() => {
                let matches = self.fn_types.get(id).is_some_and(|info| {
                    info.params == params && info.param_ownership == modes && info.result == result
                });
                if !matches {
                    self.emit(
                        span,
                        "KSEM212",
                        format!(
                            "function `{name}` has type `{}`, which does not match expected `{}`",
                            self.function_type_name(&params, &modes, result),
                            self.type_name(Type::Struct(id))
                        ),
                    );
                    return Some(self.program.exprs.alloc(HirExpr::Error));
                }
                id
            }
            Some(Type::Error) => return Some(self.program.exprs.alloc(HirExpr::Error)),
            Some(expected) => {
                self.emit(
                    span,
                    "KSEM212",
                    format!(
                        "function `{name}` is a function value, but `{}` is expected here",
                        self.type_name(expected)
                    ),
                );
                return Some(self.program.exprs.alloc(HirExpr::Error));
            }
            None => match self.function_type(params.clone(), modes.clone(), result) {
                Type::Struct(id) => id,
                _ => return Some(self.program.exprs.alloc(HirExpr::Error)),
            },
        };
        self.link_function(target, span);
        Some(self.named_function_value(repr, target, name, &params, result))
    }

    /// Builds or reuses one named function's tag and returns its function value.
    fn named_function_value(
        &mut self,
        repr: StructId,
        target: FuncId,
        name: &str,
        params: &[Type],
        result: Type,
    ) -> HirExprId {
        let tag = match self
            .fn_types
            .get(repr)
            .and_then(|info| info.named_functions.get(&target))
            .copied()
        {
            Some(tag) => tag,
            None => {
                let tag = self
                    .fn_types
                    .get(repr)
                    .map_or(0, |info| info.impls.len() as u32);
                let wrapper = self.named_function_wrapper(repr, target, name, params, result, tag);
                if let Some(info) = self.fn_types.get_mut(repr) {
                    info.impls.push(ClosureImpl { function: wrapper });
                    info.named_functions.insert(target, tag);
                }
                tag
            }
        };
        let tag_expr = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        let expr = self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: repr,
            fields: vec![tag_expr],
        });
        self.closure_sites.push(ClosureSite {
            expr,
            repr,
            tag,
            capture_fields: Vec::new(),
            capture_values: Vec::new(),
        });
        expr
    }

    /// Synthesizes the environment-taking adapter the closure dispatcher calls.
    fn named_function_wrapper(
        &mut self,
        repr: StructId,
        target: FuncId,
        name: &str,
        params: &[Type],
        result: Type,
        tag: u32,
    ) -> FuncId {
        let wrapper = self.reserve_synth();
        let mut ctx = FnCtx::new(result);
        ctx.declare_hidden(Type::Struct(repr), false);
        let mut args = Vec::with_capacity(params.len());
        for &ty in params {
            let local = ctx.declare_hidden(ty, false);
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: kira_semantics_model::hir::Callee::User(target),
            args,
            ty: result,
            writebacks: Vec::new(),
        });
        let body = if result == Type::Void {
            vec![
                self.program.stmts.alloc(HirStmt::Expr { expr: call }),
                self.program.stmts.alloc(HirStmt::Return { value: None }),
            ]
        } else {
            vec![
                self.program
                    .stmts
                    .alloc(HirStmt::Return { value: Some(call) }),
            ]
        };
        self.fill_synth(
            wrapper,
            HirFunction {
                name: format!("{name}$reference#{tag}"),
                param_count: 1 + params.len() as u32,
                return_type: result,
                locals: ctx.locals,
                body,
                is_main: false,
                execution: kira_semantics_model::Execution::Inherited,
                mutates_self: false,
                name_span: Span::new(0, 0),
            },
        );
        wrapper
    }

    /// Lifts one closure literal to a top-level function and builds its value.
    fn lift_closure(
        &mut self,
        ctx: &mut FnCtx,
        repr: StructId,
        param_types: &[Type],
        result: Type,
        params: &[ClosureParam],
        body: &Block,
    ) -> HirExprId {
        let function = self.reserve_synth();
        let tag = self
            .fn_types
            .get(repr)
            .map_or(0, |info| info.impls.len() as u32);

        let mut inner = FnCtx::new(result);
        // Slot 0 is the closure value itself. It is bound to no name, so the
        // body cannot reach it: captures arrive through the prologue below,
        // which is the only thing that reads it.
        let env = inner.declare_hidden(Type::Struct(repr), false);
        // A closure body resolves no bare field names: `self` belongs to the
        // enclosing method's frame, and the lifted function has no receiver to
        // read one from. A body that writes one gets "undefined name", which is
        // accurate — the closure genuinely has no `self`.
        inner.receiver = None;
        for (param, &ty) in params.iter().zip(param_types) {
            let name = self.interner.resolve(param.name).to_owned();
            inner.declare_param(&name, ty, false, OwnershipMode::Owned);
        }
        inner.closure = Some(ClosureCtx {
            repr,
            tag,
            captures: Vec::new(),
        });

        // The enclosing frame moves into the inner one for the duration, which
        // is what lets a name resolve outward through any depth of nesting and
        // thread a capture back through every frame it crossed.
        inner.enclosing = Some(Box::new(std::mem::replace(ctx, FnCtx::new(Type::Void))));
        let analyzed = self.analyze_block(&mut inner, body);
        if let Some(outer) = inner.enclosing.take() {
            *ctx = *outer;
        }

        let closure = inner.closure.take().unwrap_or(ClosureCtx {
            repr,
            tag,
            captures: Vec::new(),
        });
        // Each capture is bound at entry by reading its field out of the
        // closure value. Building the prologue here rather than at each use is
        // what keeps a capture read as cheap as a local read.
        let mut stmts = Vec::with_capacity(closure.captures.len() + analyzed.len());
        for capture in &closure.captures {
            let ty = inner.local_type(capture.inner);
            let base = self.program.exprs.alloc(HirExpr::Local {
                local: env,
                ty: Type::Struct(repr),
            });
            let init = self.program.exprs.alloc(HirExpr::Field {
                base,
                index: capture.field,
                ty,
            });
            stmts.push(self.program.stmts.alloc(HirStmt::Let {
                local: capture.inner,
                init,
            }));
        }
        stmts.extend(analyzed);

        let param_count = 1 + params.len() as u32;
        let name = format!("{}#{tag}", self.type_name(Type::Struct(repr)));
        let execution = self.current_execution;
        self.fill_synth(
            function,
            HirFunction {
                name,
                param_count,
                return_type: result,
                locals: inner.locals,
                body: stmts,
                is_main: false,
                execution,
                mutates_self: false,
                name_span: body.span,
            },
        );

        let capture_fields: Vec<u32> = closure.captures.iter().map(|c| c.field).collect();
        if let Some(info) = self.fn_types.get_mut(repr) {
            info.impls.push(ClosureImpl { function });
        }

        // The value: the tag plus this literal's captures, read in the frame
        // that is creating it. The remaining fields belong to other literals of
        // the same type and are filled with zeros once the list stops growing.
        let tag_expr = self.program.exprs.alloc(HirExpr::Int(i64::from(tag)));
        let mut capture_values = Vec::with_capacity(closure.captures.len());
        for capture in &closure.captures {
            let ty = ctx.local_type(capture.outer);
            capture_values.push(self.program.exprs.alloc(HirExpr::Local {
                local: capture.outer,
                ty,
            }));
        }
        let expr = self.program.exprs.alloc(HirExpr::StructNew {
            struct_id: repr,
            fields: vec![tag_expr],
        });
        self.closure_sites.push(ClosureSite {
            expr,
            repr,
            tag,
            capture_fields,
            capture_values,
        });
        expr
    }

    /// Resolves `name` in `ctx`, capturing it out of an enclosing closure frame
    /// when that is where it lives.
    ///
    /// The recursion threads a capture through *every* frame it crosses, so a
    /// closure nested two deep reads a binding of the outermost function
    /// through the closure between them rather than reaching past it.
    pub(crate) fn resolve_capturing(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        span: Span,
    ) -> Captured {
        if let Some(local) = ctx.resolve(name) {
            return Captured::Local(local);
        }
        let Some(mut outer) = ctx.enclosing.take() else {
            return Captured::Absent;
        };
        let found = self.resolve_capturing(&mut outer, name, span);
        let outcome = match found {
            Captured::Local(outer_local) => self.capture(ctx, &mut outer, outer_local, name, span),
            other => other,
        };
        ctx.enclosing = Some(outer);
        outcome
    }

    /// Threads one binding of `outer` into `ctx` as a capture.
    fn capture(
        &mut self,
        ctx: &mut FnCtx,
        outer: &mut FnCtx,
        outer_local: LocalId,
        name: &str,
        span: Span,
    ) -> Captured {
        let Some(closure) = ctx.closure.as_ref() else {
            // A frame with an enclosing frame but no closure state is not a
            // closure body, so there is nothing to capture into.
            return Captured::Absent;
        };
        if let Some(existing) = closure
            .captures
            .iter()
            .find(|capture| capture.outer == outer_local)
        {
            return Captured::Local(existing.inner);
        }
        let repr = closure.repr;
        // A capture is a read of the enclosing binding, so it answers to the
        // move checker exactly as a bare mention of the name does. Checking
        // here rather than in `typeck` is what makes it reach the *outer*
        // binding: the body only ever sees the fresh inner one, which was never
        // moved out of.
        if !self.check_local_live(outer, outer_local, span) {
            return Captured::Refused;
        }
        let ty = outer.local_type(outer_local);
        if outer.is_mutable(outer_local) {
            self.emit(
                span,
                "KSEM117",
                format!(
                    "closure captures `{name}`, which is declared `var`; a closure may only \
                     capture immutable bindings, because capturing a mutable one would share \
                     storage and nothing in this runtime shares storage yet"
                ),
            );
            return Captured::Refused;
        }
        if !super::is_trivially_copyable(ty) {
            self.emit(
                span,
                "KSEM117",
                format!(
                    "closure captures `{name}` of type `{}`, which is not trivially copyable",
                    self.type_name(ty)
                ),
            );
            return Captured::Refused;
        }
        // The literal's tag is in the name because one representation struct
        // holds the captures of *every* literal of its type: two literals that
        // each capture an `x` first would otherwise mint two fields named `x$0`
        // in one `StructDef`, and the rest of the workspace reads a struct's
        // field names as unique.
        let tag = ctx.closure.as_ref().map_or(0, |c| c.tag);
        let Some(field) = self.program.types.structs_mut().push_field(
            repr,
            FieldDef {
                name: format!(
                    "{name}${tag}_{}",
                    ctx.closure.as_ref().map_or(0, |c| c.captures.len())
                ),
                ty,
                mutable: false,
            },
        ) else {
            return Captured::Refused;
        };
        // Bound in the closure's outermost scope, so it is visible everywhere
        // in the body *and* can be shadowed by an inner declaration — which is
        // what makes a name usable before an inner `let` of the same name and
        // rebound after it.
        let inner = ctx.declare_capture(name, ty);
        // The capture stands in for the outer binding, so a jump from a use
        // inside the closure lands where that binding was written.
        if let Some(binding) = outer.binding_span(outer_local) {
            ctx.note_binding_span(inner, binding);
        }
        if let Some(closure) = ctx.closure.as_mut() {
            closure.captures.push(Capture {
                outer: outer_local,
                inner,
                field,
            });
        }
        Captured::Local(inner)
    }
}
