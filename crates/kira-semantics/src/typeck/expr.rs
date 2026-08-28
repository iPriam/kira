use super::*;
use crate::classes::Qualifier;
use crate::operators::{resolve_unary, unary_spelling};
use kira_syntax_model::ast::{CallArg, Expr};

impl Analyzer<'_> {
    pub(super) fn analyze_expr_inner(
        &mut self,
        ctx: &mut FnCtx,
        id: ExprId,
        expected: Option<Type>,
    ) -> HirExprId {
        let node = self.tree.expr(id).clone();
        match node {
            Expr::Int { value, .. } => self.program.exprs.alloc(HirExpr::Int(value)),
            Expr::Float { value, .. } => self.program.exprs.alloc(HirExpr::Float(value)),
            Expr::Bool { value, .. } => self.program.exprs.alloc(HirExpr::Bool(value)),
            Expr::Str { value, .. } => self.program.exprs.alloc(HirExpr::Str(value)),
            // `move xs` / `copy xs` sits where its operand sits, so whatever
            // was expected of the transfer is expected of what it transfers.
            Expr::Ownership { op, operand, span } => {
                self.analyze_ownership_expr(ctx, op, operand, span, expected)
            }
            // Reaching a `try` *here* means it is not the whole initializer of a
            // `let` directly inside an `attempt` body — the one position
            // `stmt::attempts` intercepts, and the only one the reference
            // pins. The operand is still analyzed so its own mistakes surface.
            Expr::Try { value, span } => {
                self.analyze_expr(ctx, value);
                self.emit(
                    span,
                    "KSEM137",
                    "`try` is only allowed as the initializer of a `let` directly inside an \
                     `attempt` body"
                        .to_owned(),
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
            Expr::ArrayLit { elements, span } => {
                self.analyze_array_literal(ctx, &elements, span, expected)
            }
            Expr::Index { base, index, span } => self.analyze_index(ctx, base, index, span),
            Expr::DotMember {
                name,
                name_span,
                args,
                span,
            } => self.analyze_dot_member(ctx, name, name_span, &args, span, expected),
            Expr::Closure {
                ref params,
                ref body,
                span,
            } => self.analyze_closure(ctx, params, body, span, expected),
            Expr::Name { symbol, span } => {
                let name = self.interner.resolve(symbol).to_owned();
                // A name that lives in an enclosing closure frame is captured
                // here, on the one path every read of a name passes through.
                match self.resolve_capturing(ctx, &name, span) {
                    crate::closures::Captured::Refused => self.program.exprs.alloc(HirExpr::Error),
                    crate::closures::Captured::Local(local) => {
                        // Reading a moved-out local is the first of KSEM107's
                        // three messages, and it is checked here — at the one
                        // place every read of a local passes through — rather
                        // than at each construct that might contain one.
                        if !self.check_local_live(ctx, local, span) {
                            return self.program.exprs.alloc(HirExpr::Error);
                        }
                        if let Some(binding) = ctx.binding_span(local) {
                            let definition = kira_source::FileSpan::new(self.source, binding);
                            self.link(span, definition);
                        }
                        // A boxed `var` reads through its box, so nothing past
                        // this point learns the box exists.
                        self.read_local(ctx, local)
                    }
                    // A local wins over a field of the same name: the nearer
                    // binding is what a reader expects, and it is what lets a
                    // method take a parameter named like a field.
                    crate::closures::Captured::Absent => {
                        match self.implicit_field(ctx, &name, span) {
                            Some(expr) => {
                                // A bare field read inside a method resolves to
                                // the receiver's field, so a jump from it lands on
                                // that field's declaration.
                                if let Some(owner) = ctx.receiver.and_then(|owner| {
                                    self.program
                                        .types
                                        .structs()
                                        .get(owner)
                                        .map(|def| def.name.clone())
                                }) {
                                    self.link_field_name(&owner, &name, span);
                                }
                                expr
                            }
                            None => {
                                // Constants share the value namespace with
                                // functions and clashes were refused, so the
                                // two lookups can run in either order.
                                if let Some(read) = self.constant_read(&name, span) {
                                    return read;
                                }
                                if let Some(reference) =
                                    self.analyze_named_function_reference(&name, span, expected)
                                {
                                    return reference;
                                }
                                // A name several parents declare is inherited but
                                // unresolvable, which is a different mistake from
                                // one nobody declared — and a different fix.
                                if !ctx.receiver.is_some_and(|owner| {
                                    self.report_ambiguous_member(owner, &name, span, false)
                                }) {
                                    self.emit(span, "KSEM060", format!("undefined name `{name}`"));
                                }
                                self.program.exprs.alloc(HirExpr::Error)
                            }
                        }
                    }
                }
            }
            Expr::Unary { op, operand, span } => {
                let operand_hir = self.analyze_expr(ctx, operand);
                let operand_ty = self.program.expr(operand_hir).type_of();
                if operand_ty == Type::Error {
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                match resolve_unary(op, operand_ty) {
                    Some((hir_op, ty)) => self.program.exprs.alloc(HirExpr::Unary {
                        op: hir_op,
                        operand: operand_hir,
                        ty,
                    }),
                    None => {
                        self.emit(
                            span,
                            "KSEM070",
                            format!(
                                "operator `{}` cannot apply to `{}`",
                                unary_spelling(op),
                                self.type_name(operand_ty)
                            ),
                        );
                        self.program.exprs.alloc(HirExpr::Error)
                    }
                }
            }
            Expr::Binary { op, lhs, rhs, span } => self.analyze_binary(ctx, op, lhs, rhs, span),
            Expr::Conditional {
                cond,
                then,
                otherwise,
                span,
            } => self.analyze_conditional(ctx, cond, then, otherwise, span, expected),
            Expr::Call {
                callee,
                callee_span,
                braced,
                type_args,
                args,
                children,
                trailing_closure,
                ..
            } => {
                let name = self.interner.resolve(callee).to_owned();
                let local = ctx.resolve(&name);
                let is_construct_update = braced
                    && local.is_some_and(|local| {
                        let ty = self
                            .cell_inner(ctx, local)
                            .unwrap_or_else(|| ctx.local_type(local));
                        self.concrete_construct_id(ty).is_some()
                    });
                // Child content belongs to a construct-backed declaration alone.
                // A call that carries children but is not one is reported here,
                // once, after its children are analyzed so their own errors
                // still surface.
                let is_construct_construction = (self.construct_backed_named(&name).is_some()
                    && local.is_none())
                    || is_construct_update;
                // The callee decides what the brace was. A parameter of function
                // type at the slot the brace sits in means it was a closure
                // written without an `in`; it joins the arguments the way a
                // written trailing closure does, and the children are dropped
                // unanalyzed, having never been the reading this call meant.
                let closure_wanted = trailing_closure.as_ref().is_some_and(|trailing| {
                    !is_construct_construction
                        && self.visible_overloads(&name).iter().any(|&id| {
                            self.param_types(id)
                                .get(trailing.slot as usize)
                                .is_some_and(|&ty| self.as_function_type(ty).is_some())
                        })
                });
                let (args, children) = match trailing_closure {
                    Some(trailing) if closure_wanted => {
                        let mut args = args;
                        let slot = (trailing.slot as usize).min(args.len());
                        let taken = (trailing.content_args as usize).min(args.len() - slot);
                        args.splice(
                            slot..slot + taken,
                            [CallArg {
                                label: None,
                                label_span: None,
                                value: trailing.closure,
                                span: self.tree.expr(trailing.closure).span(),
                            }],
                        );
                        (args, Vec::new())
                    }
                    _ => (args, children),
                };
                // A free function takes a trailing block when it declared a
                // content parameter, exactly as a construction and a method do.
                // The rule is one rule — a callable whose LAST parameter is
                // `some X` takes its children from a block — and a function left
                // out of it would be the one caller shape that has to name a
                // container for no reason but where it was declared.
                let function_content = self
                    .lookup_function(&name)
                    .map(|(id, _, _)| id)
                    .and_then(|id| self.init_content_param(id));
                if !children.is_empty() && !is_construct_construction && function_content.is_none()
                {
                    for &child in &children {
                        self.analyze_expr(ctx, child);
                    }
                    self.emit(
                        callee_span,
                        "KSEM233",
                        format!(
                            "`{name}` is not a construct-backed declaration and declares no \
                             content parameter, so it takes no trailing child content"
                        ),
                    );
                }
                if let Some(intrinsic) =
                    self.analyze_native_state_intrinsic(ctx, &name, &type_args, &args, callee_span)
                {
                    return intrinsic;
                }
                if let Some(intrinsic) =
                    self.analyze_file_system_intrinsic(ctx, &name, &type_args, &args, callee_span)
                {
                    return intrinsic;
                }
                if let Some(intrinsic) =
                    self.analyze_compiler_intrinsic(ctx, &name, &type_args, &args, callee_span)
                {
                    return intrinsic;
                }
                if let Some(intrinsic) =
                    self.analyze_env_intrinsic(ctx, &name, &type_args, &args, callee_span)
                {
                    return intrinsic;
                }
                if !type_args.is_empty()
                    && !self.is_generic_function(&name)
                    && !self.is_generic_aggregate(&name)
                {
                    self.emit(
                        callee_span,
                        "KSEM222",
                        format!("function `{name}` does not take explicit type arguments"),
                    );
                }
                // The value paths below bind by position, not by parameter
                // name; only a user function or method exposes names to bind a
                // label against. Each of those paths keeps the written values
                // and refuses a label it cannot honor.
                let values = Self::argument_values(&args);
                // A binding of function type is called by naming it, and the
                // binding wins over a function of the same name for the same
                // reason a local wins over a field: the nearer name is the one
                // a reader means.
                if is_construct_update && let Some(local) = local {
                    return self.analyze_construct_update(
                        ctx,
                        local,
                        &args,
                        &children,
                        callee_span,
                    );
                }
                if let Some(call) =
                    self.analyze_local_closure_call(ctx, &name, &values, callee_span)
                {
                    return call;
                }
                // A module constant of function type is called the same way a
                // local binding is; a local shadows it, so it is tried second.
                if ctx.resolve(&name).is_none()
                    && let Some(call) =
                        self.analyze_constant_closure_call(ctx, &name, &values, callee_span)
                {
                    return call;
                }
                // Generic aggregates use their concrete specialization as the
                // constructor target. The expected type is important for an
                // empty/defaulted constructor; positional values provide the
                // inference fallback for `Box(1)`.
                if local.is_none() && self.is_generic_aggregate(&name) {
                    let Some(id) = self.generic_aggregate_for_call(
                        ctx,
                        &name,
                        &type_args,
                        &args,
                        expected,
                        callee_span,
                    ) else {
                        for arg in &args {
                            self.analyze_expr(ctx, arg.value);
                        }
                        return self.program.exprs.alloc(HirExpr::Error);
                    };
                    self.link_type_name(&name, callee_span);
                    if self.classes.contains_key(&id) {
                        return self.analyze_class_new(ctx, id, &values, callee_span);
                    }
                    return self.analyze_struct_memberwise_new(ctx, id, &args, callee_span);
                }
                // A class is constructed by calling it, so a call whose callee
                // names a class is a constructor, not a function call.
                if let Some(id) = self.class_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    // A constructor fills fields by position; binding them by
                    // name is not supported on this surface yet.
                    return self.analyze_class_new(ctx, id, &values, callee_span);
                }
                // A construct-backed declaration is constructed by calling it,
                // like a class — but its params carry names, so a labeled
                // argument binds to the input of that name.
                if let Some(id) = self.construct_backed_named(&name)
                    && local.is_none()
                {
                    self.link_type_name(&name, callee_span);
                    return self.analyze_construction(ctx, id, &args, &children, callee_span);
                }
                // Empty bare braces are also the spelling of an empty data
                // struct literal. The parser keeps the braces on the call so
                // this choice can happen after name resolution; a construct or
                // a local construct value took the paths above.
                if braced && local.is_none() && self.plain_struct_named(&name).is_some() {
                    if !args.is_empty() {
                        for arg in &args {
                            self.analyze_expr(ctx, arg.value);
                        }
                        self.emit(
                            callee_span,
                            "KSEM269",
                            format!(
                                "plain struct `{name}` does not accept `let` construction overrides"
                            ),
                        );
                        return self.program.exprs.alloc(HirExpr::Error);
                    }
                    return self.analyze_struct_literal(ctx, callee, callee_span, &[]);
                }
                // `StructType()` on a `@FFI.Struct { layout: c }` is the zeroed-value
                // form: it takes no arguments and every field takes its zero.
                // Field initializers are written `StructType { field: value }` instead.
                if let Some(id) = self.ffi_c_layout_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    if !values.is_empty() {
                        let struct_name = self.program.types.type_name(Type::Struct(id));
                        for &value in &values {
                            self.analyze_expr(ctx, value);
                        }
                        self.emit(
                            callee_span,
                            "KSEM189",
                            format!(
                                "C-layout `{struct_name}` takes no positional arguments: write \
                                 `{struct_name}()` for a zeroed value or `{struct_name} {{ field: \
                                 value }}` to initialize fields"
                            ),
                        );
                        return self.program.exprs.alloc(HirExpr::Error);
                    }
                    return self.ffi_zero_filled_struct(id, callee_span);
                }
                // A data struct is constructed by naming it: `Point(x, y)` fills
                // its fields in declaration order and `Point(x: .., y: ..)` binds
                // each by name — the struct's implicit memberwise constructor,
                // the two spellings the `Point { x: .., y: .. }` literal already
                // has. Recognized before the undefined-function path so a
                // construction is never reported as a missing function.
                if let Some(id) = self.plain_struct_named(&name)
                    && ctx.resolve(&name).is_none()
                {
                    self.link_type_name(&name, callee_span);
                    return self.analyze_struct_memberwise_new(ctx, id, &args, callee_span);
                }
                // A bare call inside a method may name one of the receiver's
                // own or inherited methods, the way a bare name may read one of
                // its fields. A method exposes parameter names, so labels flow
                // through unchanged.
                if let Some(call) = self.implicit_method_call(ctx, &name, &args, callee_span) {
                    return call;
                }
                // `Int(x)` / `U32(x)` / `Float(x)` and the rest of the numeric
                // scalar set is a value conversion, not a call — recognized here
                // before the undefined-function path so a cast is never reported
                // as a missing function.
                if let Some(call) = self.analyze_bit_reinterpret(ctx, &name, &args, callee_span) {
                    return call;
                }
                if let Some(call) = self.analyze_scalar_conversion(ctx, &name, &args, callee_span) {
                    return call;
                }
                // `String(x)` renders a scalar as text, and is recognized here
                // for the same reason: a conversion is never an undefined
                // function.
                if let Some(call) = self.analyze_string_conversion(ctx, &name, &args, callee_span) {
                    return call;
                }
                if let Some(call) = self.analyze_raw_pointer_word(ctx, &name, &args, callee_span) {
                    return call;
                }
                // `taskYield()` / `taskSleep(ms)` are the executor's two
                // suspend points. They are builtins rather than library
                // functions because the compiler has to *see* them: a call to
                // one is where the drive loop gets its turn.
                if let Some(call) = self.analyze_task_builtin(ctx, &name, &values, callee_span) {
                    return call;
                }
                if name == "print" {
                    // `print` borrows: it renders its argument and consumes
                    // nothing the caller could miss.
                    let arg_hirs: Vec<HirExprId> = values
                        .iter()
                        .map(|&arg| self.analyze_expr(ctx, arg))
                        .collect();
                    self.analyze_print(&arg_hirs, callee_span)
                } else if let Some(id) = self.foreign_named(&name) {
                    // A bare call whose name is a recorded `@FFI.Extern`
                    // callable is an ordinary Kira call — no `@Native`, no
                    // ceremony — resolved to `Callee::Foreign`.
                    self.analyze_foreign_call(ctx, id, &values, callee_span)
                } else {
                    let trailing = match function_content {
                        Some(content) if !children.is_empty() => {
                            vec![self.content_value(ctx, &content, &children, callee_span)]
                        }
                        _ => Vec::new(),
                    };
                    self.analyze_user_call_from_syntax_with_type_args(calls::CallSyntax {
                        ctx,
                        name: &name,
                        leading: &[],
                        type_args: &type_args,
                        args: &args,
                        trailing: &trailing,
                        span: callee_span,
                    })
                }
            }
            Expr::StructLit {
                name,
                name_span,
                fields,
                span,
            } => {
                // A local concrete construct value may shadow the family/type
                // name. Its `Name { field = value }` form is the canonical
                // component-style update, even though the parser must retain
                // the same AST shape as an ordinary struct literal to keep
                // `Color { r = ... }` unambiguous.
                let written = self.interner.resolve(name).to_owned();
                let local = ctx.resolve(&written);
                let is_construct_update = local.is_some_and(|local| {
                    let ty = self
                        .cell_inner(ctx, local)
                        .unwrap_or_else(|| ctx.local_type(local));
                    self.concrete_construct_id(ty).is_some()
                });
                if let Some(local) = local.filter(|_| is_construct_update) {
                    let args: Vec<CallArg> = fields
                        .iter()
                        .map(|field| CallArg {
                            label: Some(field.name),
                            label_span: Some(field.name_span),
                            value: field.value,
                            span: field.span,
                        })
                        .collect();
                    return self.analyze_construct_update(ctx, local, &args, &[], span);
                }
                self.analyze_struct_literal(ctx, name, name_span, &fields)
            }
            Expr::Field {
                base,
                field,
                field_span,
                span,
            } => {
                let name = self.interner.resolve(field).to_owned();
                // `ClsAlpha.v` reads a parent's field through `self`; the base
                // is a type name, not a value, so it must be recognized before
                // anything tries to analyze it as one.
                match self.parent_qualifier_of(ctx, base) {
                    Qualifier::Parent(qualifier) => {
                        return self.analyze_parent_field(ctx, qualifier, &name, field_span);
                    }
                    // The qualifier was a type name and it did not apply here;
                    // that was already reported, so say nothing more about it.
                    Qualifier::Rejected => {
                        return self.program.exprs.alloc(HirExpr::Error);
                    }
                    Qualifier::NotAType => {}
                }
                // `SizeMode.Hug` / `Foundation.SizeMode.Fill` is a payload-less
                // enum variant written with a qualified spelling rather than a
                // leading dot — the base names the enum, `field` names the
                // variant. Recognized before the base is analyzed as a value,
                // because an enum name is not one.
                match self.qualified_enum_at(ctx, base, expected) {
                    crate::enums::QualifiedEnum::Enum(enum_id) => {
                        return self.analyze_dot_member(
                            ctx,
                            field,
                            field_span,
                            &None,
                            span,
                            Some(Type::Enum(enum_id)),
                        );
                    }
                    // `Result.Ok` where the position asks for no instantiation
                    // of `Result`. The base is a template, not a value, so
                    // analyzing it as one would report an undefined name.
                    crate::enums::QualifiedEnum::Unanchored(template) => {
                        return self.report_unanchored_generic_construction(
                            ctx, &template, field, &None, span, expected,
                        );
                    }
                    crate::enums::QualifiedEnum::NotAnEnum => {}
                }
                let base_hir = self.analyze_expr(ctx, base);
                let base_ty = self.program.expr(base_hir).type_of();
                // `handle.await` is the one property a task handle has, and the
                // handle is opaque, so anything else read off one is refused
                // here rather than falling through to a field lookup that would
                // report the wrong thing.
                if let Type::Task(result) = base_ty {
                    return self.analyze_task_property(base_hir, result, &name, field_span);
                }
                // An array has no fields, but it does have `.count` — a
                // property, written with the same syntax a field read uses.
                if base_ty.is_array() {
                    return self.analyze_array_property(base_hir, &name, field_span);
                }
                // A `String` has no fields either, and the same one property:
                // its byte count, written exactly as an array's is.
                if base_ty == Type::String {
                    return self.analyze_string_property(base_hir, &name, field_span);
                }
                if let Type::Enum(family_id) = base_ty
                    && self.construct_family_computed_member(family_id, &name)
                {
                    return self.analyze_construct_family_property(
                        ctx, base_hir, base, family_id, &name, field_span,
                    );
                }
                // A value member of the family — `@Required let` or typed
                // stored member — is read the same way, but dispatches to a
                // stored field or a computed member depending on what each
                // backed declaration chose to satisfy it with.
                if let Type::Enum(family_id) = base_ty
                    && let Some(result) = self.construct_family_field_member(family_id, &name)
                {
                    return self.analyze_construct_family_field(base_hir, family_id, &name, result);
                }
                // A stored family member with no written type cannot be read
                // through the family value: nothing says what the read returns.
                // Named here so the fix — declare the type on the family — is
                // visible, instead of the generic "has no fields".
                if let Type::Enum(family_id) = base_ty
                    && let Some(family) = self.construct_family_untyped_member(family_id, &name)
                {
                    self.emit(
                        field_span,
                        "KSEM271",
                        format!(
                            "`{name}` on construct family `{family}` declares no type, so it \
                             cannot be read through `Any {family}`; declare it as `let {name}: \
                             T = …` on the family"
                        ),
                    );
                    return self.program.exprs.alloc(HirExpr::Error);
                }
                // A construct's computed bridge member (`value.node`) is read as
                // a property but runs the member, so it lowers to a method call
                // rather than a field read.
                if let Type::Struct(id) = base_ty
                    && self.construct_computed_member(id, &name)
                {
                    return self
                        .analyze_construct_bridge_read(ctx, base_hir, id, &name, field_span);
                }
                // A member reached through an `@FFI.Pointer` resolves against
                // the target's C layout rather than a Kira value's fields, and
                // lowers to a load or to the member's address.
                if let Type::ForeignPtr(pointer) = base_ty {
                    return self.analyze_foreign_field(base_hir, pointer, &name, field_span);
                }
                match self.resolve_field(base_ty, &name, field_span) {
                    Some((index, ty)) => {
                        if let Type::Struct(id) = base_ty
                            && let Some(owner) = self
                                .program
                                .types
                                .structs()
                                .get(id)
                                .map(|def| def.name.clone())
                        {
                            self.link_field_name(&owner, &name, field_span);
                        }
                        let read = self.program.exprs.alloc(HirExpr::Field {
                            base: base_hir,
                            index,
                            ty,
                        });
                        // The base is read without being consumed, whatever it
                        // held; the read itself is a new value of `ty`, which
                        // is where a user `Drop` body would run a second time.
                        self.excuse_drop_extraction(base_hir);
                        self.note_drop_extraction(read, field_span);
                        read
                    }
                    None => self.program.exprs.alloc(HirExpr::Error),
                }
            }
            Expr::MethodCall {
                receiver,
                method,
                method_span,
                args,
                children,
                ..
            } => self.analyze_method_call(
                ctx,
                receiver,
                method,
                method_span,
                calls::MethodCallContent {
                    args: &args,
                    children: &children,
                },
                expected,
            ),
            // A bare `{ … }` block is the anonymous spelling of a named child
            // fill, so it is a value nowhere else. Its children are analyzed so
            // their own mistakes surface before it is refused.
            // Unless the position asks for a function: then the same brace is a
            // closure the author wrote without an `in`. The parser carried both
            // readings because only the expectation tells them apart, and here
            // it is.
            Expr::Content {
                closure: Some(closure),
                ..
            } if expected.is_some_and(|ty| self.as_function_type(ty).is_some()) => {
                self.analyze_expr_expecting(ctx, closure, expected)
            }
            Expr::Content {
                ref children, span, ..
            } => {
                let children = children.clone();
                for &child in &children {
                    self.analyze_expr(ctx, child);
                }
                self.emit(
                    span,
                    "KSEM273",
                    "a `{ … }` content block fills a child slot by name, so it is not a value here",
                );
                self.program.exprs.alloc(HirExpr::Error)
            }
            // A `For`/`if` builder only ever reaches analysis as a construction's
            // content child, where [`fill_child_slots`] expands it or refuses
            // the surrounding block (`KSEM229`/`KSEM242`). Reaching ordinary
            // expression analysis means the surrounding construction was already
            // rejected for another reason; its sub-expressions are still
            // analyzed so their own mistakes surface, then it stands in with an
            // error value rather than adding a second, vaguer message.
            Expr::ContentFor {
                iterable, ref body, ..
            } => {
                self.analyze_expr(ctx, iterable);
                for &item in body {
                    self.analyze_expr(ctx, item);
                }
                self.program.exprs.alloc(HirExpr::Error)
            }
            Expr::ContentIf {
                cond,
                ref then_body,
                ref else_body,
                ..
            } => {
                self.analyze_expr(ctx, cond);
                for &item in then_body.iter().chain(else_body) {
                    self.analyze_expr(ctx, item);
                }
                self.program.exprs.alloc(HirExpr::Error)
            }
            Expr::TaskSpawn { body, span } => self.analyze_task_spawn(ctx, body, span),
            Expr::Error { .. } => self.program.exprs.alloc(HirExpr::Error),
        }
    }
}
