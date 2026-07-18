//! Calling a closure value, and finishing the desugar once analysis is done.

use kira_semantics_model::hir::{
    Callee, FuncId, HirBinaryOp, HirExpr, HirExprId, HirFunction, HirStmt, HirStmtId, LocalId,
};
use kira_semantics_model::{OwnershipMode, StructId, Type};
use kira_source::Span;
use kira_syntax_model::ast::ExprId;

use crate::analyze::{Analyzer, FnCtx};

impl Analyzer<'_> {
    /// Type-checks `f(args)` when `f` names a binding of function type.
    ///
    /// `None` when it names no such binding, so the caller carries on treating
    /// the call as a class construction or a free function call.
    pub(crate) fn analyze_local_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let local = match self.resolve_capturing(ctx, name, span) {
            super::Captured::Local(local) => local,
            // A refused capture already spoke; calling it would say it twice.
            super::Captured::Refused => return Some(self.program.exprs.alloc(HirExpr::Error)),
            super::Captured::Absent => return None,
        };
        let ty = ctx.local_type(local);
        let repr = self.as_function_type(ty)?;
        if !self.check_local_live(ctx, local, span) {
            return Some(self.program.exprs.alloc(HirExpr::Error));
        }
        let expr = self.program.exprs.alloc(HirExpr::Local { local, ty });
        Some(self.analyze_closure_call(ctx, expr, repr, args, span))
    }

    /// Type-checks `receiver.name(args)` when `name` is a field of function
    /// type rather than a method.
    ///
    /// A closure stored in a field is called through the field, which is the
    /// same syntax a method call uses — so this is tried before "no such
    /// method" is reported.
    pub(crate) fn analyze_field_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        receiver: HirExprId,
        receiver_ty: Type,
        name: &str,
        args: &[ExprId],
        span: Span,
    ) -> Option<HirExprId> {
        let Type::Struct(owner) = receiver_ty else {
            return None;
        };
        let def = self.program.types.structs().get(owner)?;
        let index = def.field_index(name)?;
        let field_ty = def.field(index)?.ty;
        let repr = self.as_function_type(field_ty)?;
        let expr = self.program.exprs.alloc(HirExpr::Field {
            base: receiver,
            index,
            ty: field_ty,
        });
        Some(self.analyze_closure_call(ctx, expr, repr, args, span))
    }

    /// Type-checks a call *through* a closure value.
    ///
    /// The value becomes the dispatcher's first argument, exactly as a method's
    /// receiver becomes its function's first parameter — which is what keeps
    /// closures out of the IR and out of every backend.
    fn analyze_closure_call(
        &mut self,
        ctx: &mut FnCtx,
        expr: HirExprId,
        repr: StructId,
        args: &[ExprId],
        span: Span,
    ) -> HirExprId {
        let Some((params, result)) = self
            .fn_types
            .get(repr)
            .map(|info| (info.params.clone(), info.result))
        else {
            return self.program.exprs.alloc(HirExpr::Error);
        };
        let dispatcher = self.dispatcher_for(repr);
        if args.len() != params.len() {
            self.emit(
                span,
                "KSEM062",
                format!(
                    "`{}` takes {} argument(s), found {}",
                    self.type_name(Type::Struct(repr)),
                    params.len(),
                    args.len()
                ),
            );
        }
        let mut all = vec![expr];
        for (index, &arg) in args.iter().enumerate() {
            match params.get(index) {
                // A closure's parameters are owned and unannotated, so each
                // argument is checked exactly as a bare parameter's is.
                Some(&expected) => {
                    let name = self.type_name(Type::Struct(repr));
                    all.push(self.analyze_call_argument(
                        ctx,
                        arg,
                        expected,
                        OwnershipMode::Owned,
                        &name,
                    ));
                }
                None => all.push(self.analyze_expr(ctx, arg)),
            }
        }
        for (index, (&arg, &expected)) in all.iter().skip(1).zip(params.iter()).enumerate() {
            let actual = self.program.expr(arg).type_of();
            if !actual.assignable_to(expected) {
                self.emit(
                    span,
                    "KSEM063",
                    format!(
                        "argument {} of `{}` expects `{}`, found `{}`",
                        index + 1,
                        self.type_name(Type::Struct(repr)),
                        self.type_name(expected),
                        self.type_name(actual)
                    ),
                );
            }
        }
        self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(dispatcher),
            args: all,
            ty: result,
        })
    }

    /// The dispatcher for a function type, reserving its id on first need.
    fn dispatcher_for(&mut self, repr: StructId) -> FuncId {
        if let Some(existing) = self.fn_types.get(repr).and_then(|info| info.dispatcher) {
            return existing;
        }
        let id = self.reserve_synth();
        if let Some(info) = self.fn_types.get_mut(repr) {
            info.dispatcher = Some(id);
        }
        id
    }

    /// Whether `ty` is a function type, and its representation struct.
    pub(crate) fn as_function_type(&self, ty: Type) -> Option<StructId> {
        match ty {
            Type::Struct(id) if self.fn_types.get(id).is_some() => Some(id),
            _ => None,
        }
    }

    /// Finishes the desugar: pads every closure value's field list and builds
    /// every dispatcher body.
    ///
    /// Runs once, after every body is analyzed, because both depend on facts
    /// that are only final then — how many literals each function type has, and
    /// how wide its representation struct grew.
    pub(crate) fn finalize_closures(&mut self) {
        self.finalize_closure_values();
        self.build_dispatchers();
        // Synthesized functions sit after every declared one, which is what
        // makes a reserved id an index into the finished list.
        let synth = std::mem::take(&mut self.synth);
        for function in synth {
            if let Some(function) = function {
                self.program.functions.push(function);
            } else {
                // A reserved id is always filled: a dispatcher is built above,
                // and a lifted body is filled where it is lifted. A hole would
                // shift every later id, so it is replaced by an empty function
                // rather than dropped.
                self.program.functions.push(HirFunction {
                    name: "<unreachable closure>".to_owned(),
                    param_count: 0,
                    return_type: Type::Void,
                    locals: Vec::new(),
                    body: Vec::new(),
                    is_main: false,
                    execution: kira_semantics_model::Execution::Inherited,
                    name_span: Span::new(0, 0),
                });
            }
        }
    }

    /// Pads each closure literal's `StructNew` out to its representation
    /// struct's full field list.
    fn finalize_closure_values(&mut self) {
        let sites = std::mem::take(&mut self.closure_sites);
        for site in &sites {
            let field_types: Vec<Type> = match self.program.types.structs().get(site.repr) {
                Some(def) => def.fields.iter().map(|field| field.ty).collect(),
                None => continue,
            };
            let mut fields = Vec::with_capacity(field_types.len());
            for (index, &ty) in field_types.iter().enumerate() {
                let index = index as u32;
                if index == 0 {
                    fields.push(self.program.exprs.alloc(HirExpr::Int(i64::from(site.tag))));
                    continue;
                }
                match site.capture_fields.iter().position(|&field| field == index) {
                    Some(slot) => fields.push(site.capture_values[slot]),
                    // A field another literal of the same type owns. This value
                    // never reads it, and the dispatcher only ever reads the
                    // fields its own tag names, so any value of the right type
                    // does.
                    None => fields.push(self.default_value(ty)),
                }
            }
            if let HirExpr::StructNew { fields: slot, .. } = &mut self.program.exprs[site.expr] {
                *slot = fields;
            }
        }
    }

    /// One dispatcher branch: forward the closure value and every parameter to
    /// the lifted body, and return what it returns.
    fn dispatch_arm(
        &mut self,
        target: FuncId,
        env: LocalId,
        repr: StructId,
        param_locals: &[LocalId],
        params: &[Type],
        result: Type,
    ) -> Vec<HirStmtId> {
        let mut args = vec![self.program.exprs.alloc(HirExpr::Local {
            local: env,
            ty: Type::Struct(repr),
        })];
        for (&local, &ty) in param_locals.iter().zip(params.iter()) {
            args.push(self.program.exprs.alloc(HirExpr::Local { local, ty }));
        }
        let call = self.program.exprs.alloc(HirExpr::Call {
            callee: Callee::User(target),
            args,
            ty: result,
        });
        if result == Type::Void {
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
        }
    }

    /// A well-typed value of `ty`, for a slot nothing reads.
    ///
    /// Every arm builds a value the backends agree is of `ty`, because a
    /// backend type-checks what it is handed: an `Int(0)` standing in for a
    /// `String` passes the VM and is rejected by the LLVM verifier, which is
    /// exactly the parity break this exists to make impossible.
    ///
    /// The recursion terminates because a type can only name types declared
    /// before it: a struct field of the struct's own type is `KSEM051`, and an
    /// enum payload of the enum's own type is `KSEM050`.
    fn default_value(&mut self, ty: Type) -> HirExprId {
        let node = match ty {
            Type::Float(_) => HirExpr::Float(0.0),
            Type::Bool => HirExpr::Bool(false),
            Type::String => HirExpr::Str(String::new()),
            Type::Array(_) => HirExpr::ArrayNew {
                ty,
                elements: Vec::new(),
            },
            Type::Struct(id) => {
                let field_types: Vec<Type> = match self.program.types.structs().get(id) {
                    Some(def) => def.fields.iter().map(|field| field.ty).collect(),
                    None => Vec::new(),
                };
                let fields = field_types
                    .into_iter()
                    .map(|field_ty| self.default_value(field_ty))
                    .collect();
                HirExpr::StructNew {
                    struct_id: id,
                    fields,
                }
            }
            Type::Enum(id) => {
                // The first variant, because an enum's variants are ordered and
                // the first one always exists for any enum a value can have.
                let payload_ty = self
                    .program
                    .types
                    .enums()
                    .get(id)
                    .and_then(|def| def.variant(0))
                    .and_then(|variant| variant.payload);
                let payload = payload_ty.map(|payload_ty| self.default_value(payload_ty));
                HirExpr::EnumNew {
                    enum_id: id,
                    tag: 0,
                    payload,
                }
            }
            // `Void` never reaches here (its callers return without a value)
            // and `Error` means the program is already rejected.
            Type::Int(_) | Type::Void | Type::Error => HirExpr::Int(0),
        };
        self.program.exprs.alloc(node)
    }

    /// Builds the body of every dispatcher a call site reserved.
    fn build_dispatchers(&mut self) {
        // Sorted by id rather than taken in table order: the table is a hash
        // map, and building bodies in its iteration order would allocate this
        // program's arena nodes in a different order on every run.
        let mut rows: Vec<(StructId, FuncId)> = self
            .fn_types
            .rows()
            .filter_map(|(id, info)| info.dispatcher.map(|dispatcher| (id, dispatcher)))
            .collect();
        rows.sort_by_key(|&(_, dispatcher)| dispatcher.0);
        for (repr, dispatcher) in rows {
            let function = self.dispatcher_body(repr);
            self.fill_synth(dispatcher, function);
        }
    }

    /// The dispatcher for one function type: a branch per closure literal.
    fn dispatcher_body(&mut self, repr: StructId) -> HirFunction {
        let Some((params, result, impls)) = self
            .fn_types
            .get(repr)
            .map(|info| (info.params.clone(), info.result, info.impls.clone()))
        else {
            return HirFunction {
                name: "<unreachable dispatcher>".to_owned(),
                param_count: 0,
                return_type: Type::Void,
                locals: Vec::new(),
                body: Vec::new(),
                is_main: false,
                execution: kira_semantics_model::Execution::Inherited,
                name_span: Span::new(0, 0),
            };
        };
        let mut ctx = FnCtx::new(result);
        let env = ctx.declare_hidden(Type::Struct(repr), false);
        let mut param_locals = Vec::with_capacity(params.len());
        for &ty in &params {
            param_locals.push(ctx.declare_hidden(ty, false));
        }

        // The last literal is the chain's unconditional tail rather than one
        // more tested branch. That is not an optimization: a tested last branch
        // leaves a fall-through the dispatcher would have to return *some*
        // value from, and no value of an arbitrary result type can be conjured
        // that every backend agrees is of that type.
        let mut body: Vec<HirStmtId> = Vec::with_capacity(impls.len().max(1));
        for (tag, closure) in impls.iter().enumerate() {
            let arm =
                self.dispatch_arm(closure.function, env, repr, &param_locals, &params, result);
            if tag + 1 == impls.len() {
                body.extend(arm);
                break;
            }
            let tag_read = {
                let base = self.program.exprs.alloc(HirExpr::Local {
                    local: env,
                    ty: Type::Struct(repr),
                });
                self.program.exprs.alloc(HirExpr::Field {
                    base,
                    index: 0,
                    ty: Type::INT,
                })
            };
            let wanted = self.program.exprs.alloc(HirExpr::Int(tag as i64));
            let cond = self.program.exprs.alloc(HirExpr::Binary {
                op: HirBinaryOp::EqInt,
                lhs: tag_read,
                rhs: wanted,
                ty: Type::Bool,
            });
            body.push(self.program.stmts.alloc(HirStmt::If {
                cond,
                then_body: arm,
                else_body: Vec::new(),
            }));
        }
        if impls.is_empty() {
            // A function type mentioned and called, but with no literal
            // anywhere in the program: nothing can build a value of it, so no
            // call can reach this. It still needs a well-typed terminator,
            // because a backend type-checks a body it can prove is dead.
            let tail = if result == Type::Void {
                HirStmt::Return { value: None }
            } else {
                let value = self.default_value(result);
                HirStmt::Return { value: Some(value) }
            };
            body.push(self.program.stmts.alloc(tail));
        }

        HirFunction {
            name: format!("{}$call", self.type_name(Type::Struct(repr))),
            param_count: 1 + params.len() as u32,
            return_type: result,
            locals: ctx.locals,
            body,
            is_main: false,
            execution: kira_semantics_model::Execution::Inherited,
            name_span: Span::new(0, 0),
        }
    }
}
